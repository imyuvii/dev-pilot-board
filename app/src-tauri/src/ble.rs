//! Bluetooth LE link to the ring — the third transport beside WebSocket and
//! USB serial. The ring advertises the Nordic UART Service (NUS): one
//! characteristic to write command lines to, one that notifies reply lines.
//! Same line protocol as the other two, so `led.rs` treats this as just
//! another `Conn`; nothing here knows about events or colours.
//!
//! Why a blocking API on top of an async crate: the LED worker is a plain
//! thread with a 120ms loop, and every other transport is blocking. Each call
//! here parks on Tauri's tokio runtime for the one await it needs, and a
//! background task turns the notification stream into `\n`-delimited lines
//! on a channel that `drain` empties without blocking.
//!
//! Optional like everything else: no adapter, Bluetooth off, permission
//! denied, ring out of range — every path is a quiet `None`/`Err` for the
//! worker's retry/backoff. macOS asks for Bluetooth permission the first time
//! a `Manager` is created, so that happens lazily and only once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use btleplug::api::{
    Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use uuid::Uuid;

pub const NUS_SERVICE: Uuid = Uuid::from_u128(0x6E400001_B5A3_F393_E0A9_E50E24DCCA9E);
const NUS_RX: Uuid = Uuid::from_u128(0x6E400002_B5A3_F393_E0A9_E50E24DCCA9E);
const NUS_TX: Uuid = Uuid::from_u128(0x6E400003_B5A3_F393_E0A9_E50E24DCCA9E);
/// The name the firmware advertises; matched as a fallback when a scan
/// response arrives before the service list does.
pub const RING_NAME: &str = "LED Ring";

/// How long one scan waits for the ring before giving up.
const SCAN_WINDOW: Duration = Duration::from_millis(4000);

static MANAGER: Mutex<Option<Manager>> = Mutex::new(None);
/// btleplug's `adapters()` spins up a fresh CoreBluetooth thread and
/// CBCentralManager on *every* call and never stops the old ones, so the
/// adapter is created once and reused across connect attempts.
static ADAPTER: Mutex<Option<Adapter>> = Mutex::new(None);
/// Every CoreBluetooth round-trip is bounded: a lost delegate callback must
/// cost one retry, never the whole LED worker (see the profile note in
/// Cargo.toml for why that is a real risk).
const CB_TIMEOUT: Duration = Duration::from_secs(5);

/// macOS's per-app Bluetooth permission, so the settings panel can say
/// "click Allow" or "blocked in System Settings" instead of "no ring".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Auth {
    NotDetermined,
    Restricted,
    Denied,
    Allowed,
}

pub fn authorization() -> Auth {
    // +[CBCentralManager authorization]; CoreBluetooth is already linked by
    // btleplug. Values per CBManagerAuthorization.
    let cls = objc2::class!(CBCentralManager);
    let v: isize = unsafe { objc2::msg_send![cls, authorization] };
    match v {
        1 => Auth::Restricted,
        2 => Auth::Denied,
        3 => Auth::Allowed,
        _ => Auth::NotDetermined,
    }
}

/// One line for the settings panel when a Bluetooth connect came up empty.
pub fn failure_note() -> String {
    match authorization() {
        Auth::NotDetermined => "Bluetooth: waiting for you to click Allow in the macOS prompt".into(),
        Auth::Denied | Auth::Restricted => {
            "Bluetooth is blocked for this app — System Settings → Privacy & Security → Bluetooth → allow Dev Pilot Board".into()
        }
        Auth::Allowed => "No ring found over Bluetooth — is it powered and within a few metres?".into(),
    }
}

/// btleplug needs a Tokio runtime with the timer and I/O drivers, and it
/// spawns its own forwarder tasks on whatever runtime is current. Tauri's
/// `async_runtime::block_on` does not enter one, so `tokio::time::timeout`
/// panicked with "there is no reactor running" the moment the first BLE
/// command was sent. Own a small multi-thread runtime here instead: it keeps
/// the CoreBluetooth forwarder tasks polled for the life of the process.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("ble")
            .build()
            .expect("tokio runtime for BLE")
    })
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    runtime().block_on(f)
}

/// Created lazily: this is what makes macOS show the Bluetooth permission
/// prompt, so it must not happen until the LED feature is on. Kept once it
/// exists; a failed or timed-out creation is retried on the next attempt.
fn manager() -> Option<Manager> {
    let mut slot = MANAGER.lock().ok()?;
    if slot.is_none() {
        let made = block_on(async {
            tokio::time::timeout(CB_TIMEOUT, Manager::new()).await.ok()?.ok()
        });
        if made.is_none() {
            dbg("CoreBluetooth manager did not come up in time");
        }
        *slot = made;
    }
    slot.clone()
}

pub struct BleLink {
    peripheral: Peripheral,
    rx: Characteristic,
    lines: Receiver<String>,
    alive: Arc<AtomicBool>,
    /// Address, for the status line.
    pub id: String,
}

/// Scan for the ring and connect. `None` covers every failure, including
/// "found nothing in 4s".
fn dbg(msg: &str) {
    if std::env::var_os("DPB_LED_DEBUG").is_some() {
        eprintln!("[led] ble: {msg}");
    }
}

pub fn connect() -> Option<BleLink> {
    dbg(&format!("authorization={:?}", authorization()));
    let Some(manager) = manager() else {
        dbg("no manager (CoreBluetooth init failed)");
        return None;
    };
    let central = {
        let mut slot = ADAPTER.lock().ok()?;
        if slot.is_none() {
            let t0 = Instant::now();
            let made = block_on(async {
                let adapters = tokio::time::timeout(CB_TIMEOUT, manager.adapters()).await.ok()?.ok()?;
                dbg(&format!("adapters={} after {:?}", adapters.len(), t0.elapsed()));
                adapters.into_iter().next()
            });
            if made.is_none() {
                dbg(&format!("adapter did not come up within {:?}", t0.elapsed()));
            }
            *slot = made;
        }
        slot.clone()?
    };
    block_on(async {
        let t0 = Instant::now();
        let mut events = tokio::time::timeout(CB_TIMEOUT, central.events()).await.ok()?.ok()?;
        tokio::time::timeout(
            CB_TIMEOUT,
            central.start_scan(ScanFilter { services: vec![NUS_SERVICE] }),
        )
        .await
        .ok()?
        .ok()?;

        let deadline = Instant::now() + SCAN_WINDOW;
        let mut found: Option<Peripheral> = None;
        let mut seen = 0usize;
        let mut why = "found";
        while found.is_none() && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let ev = match tokio::time::timeout(remaining, events.next()).await {
                Ok(Some(ev)) => ev,
                Ok(None) => {
                    why = "event stream ended";
                    break;
                }
                Err(_) => {
                    why = "scan window elapsed";
                    break;
                }
            };
            let id = match ev {
                CentralEvent::DeviceDiscovered(id)
                | CentralEvent::DeviceUpdated(id)
                | CentralEvent::ServicesAdvertisement { id, .. } => id,
                _ => continue,
            };
            seen += 1;
            let Ok(p) = central.peripheral(&id).await else { continue };
            let Ok(Some(props)) = p.properties().await else { continue };
            let by_service = props.services.contains(&NUS_SERVICE);
            let by_name = props.local_name.as_deref() == Some(RING_NAME);
            if by_service || by_name {
                found = Some(p);
            }
        }
        let _ = tokio::time::timeout(CB_TIMEOUT, central.stop_scan()).await;
        dbg(&format!(
            "scan done after {:?}: {seen} advertisements seen, ring found={} ({why})",
            t0.elapsed(),
            found.is_some()
        ));
        let peripheral = found?;

        tokio::time::timeout(Duration::from_secs(6), peripheral.connect())
            .await
            .ok()?
            .ok()?;
        tokio::time::timeout(CB_TIMEOUT, peripheral.discover_services()).await.ok()?.ok()?;
        let chars = peripheral.characteristics();
        let rx = chars.iter().find(|c| c.uuid == NUS_RX)?.clone();
        let tx = chars.iter().find(|c| c.uuid == NUS_TX)?.clone();
        tokio::time::timeout(CB_TIMEOUT, peripheral.subscribe(&tx)).await.ok()?.ok()?;
        let mut notifications = tokio::time::timeout(CB_TIMEOUT, peripheral.notifications()).await.ok()?.ok()?;

        let (send, lines) = mpsc::channel::<String>();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_task = alive.clone();
        // Reply lines end in '\n' and may be split across notifications
        // (STATE is ~170 bytes); reassemble here so the worker only ever
        // sees whole lines.
        runtime().spawn(async move {
            let mut buf = String::new();
            while let Some(n) = notifications.next().await {
                if n.uuid != NUS_TX {
                    continue;
                }
                buf.push_str(&String::from_utf8_lossy(&n.value));
                while let Some(i) = buf.find('\n') {
                    let line = buf[..i].trim_end_matches('\r').to_string();
                    buf.drain(..=i);
                    if !line.is_empty() && send.send(line).is_err() {
                        break;
                    }
                }
            }
            alive_task.store(false, Ordering::SeqCst);
        });

        let id = peripheral.id().to_string();
        Some(BleLink { peripheral, rx, lines, alive, id })
    })
}

impl BleLink {
    pub fn write_line(&self, line: &str) -> Result<(), String> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err("ble link closed".into());
        }
        block_on(async {
            tokio::time::timeout(
                CB_TIMEOUT,
                self.peripheral
                    .write(&self.rx, format!("{line}\n").as_bytes(), WriteType::WithoutResponse),
            )
            .await
        })
        .map_err(|_| "ble write timed out".to_string())?
        .map_err(|e| e.to_string())
    }

    /// Reply lines that arrived, waiting at most `window` for the first one.
    pub fn drain(&self, window: Duration) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        let deadline = Instant::now() + window;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(l) => out.push(l),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return Err("ble link closed".into()),
            }
        }
        if !self.alive.load(Ordering::SeqCst) {
            return Err("ble link closed".into());
        }
        // A dropped link does not always end the notification stream promptly.
        match block_on(async {
            tokio::time::timeout(CB_TIMEOUT, self.peripheral.is_connected()).await
        }) {
            Ok(Ok(true)) => Ok(out),
            _ => Err("ble disconnected".into()),
        }
    }
}

impl Drop for BleLink {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        let _ = block_on(async {
            tokio::time::timeout(CB_TIMEOUT, self.peripheral.disconnect()).await
        });
    }
}
