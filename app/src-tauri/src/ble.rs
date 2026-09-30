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
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use btleplug::api::{
    Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
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

static MANAGER: OnceLock<Option<Manager>> = OnceLock::new();

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tauri::async_runtime::block_on(f)
}

/// Created once per process: this is what makes macOS show the Bluetooth
/// permission prompt, so it must not happen until the LED feature is on.
fn manager() -> Option<&'static Manager> {
    MANAGER
        .get_or_init(|| block_on(async { Manager::new().await.ok() }))
        .as_ref()
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
pub fn connect() -> Option<BleLink> {
    let manager = manager()?;
    block_on(async {
        let adapters = manager.adapters().await.ok()?;
        let central = adapters.into_iter().next()?;
        let mut events = central.events().await.ok()?;
        central
            .start_scan(ScanFilter { services: vec![NUS_SERVICE] })
            .await
            .ok()?;

        let deadline = Instant::now() + SCAN_WINDOW;
        let mut found: Option<Peripheral> = None;
        while found.is_none() && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let ev = match tokio::time::timeout(remaining, events.next()).await {
                Ok(Some(ev)) => ev,
                _ => break,
            };
            let id = match ev {
                CentralEvent::DeviceDiscovered(id)
                | CentralEvent::DeviceUpdated(id)
                | CentralEvent::ServicesAdvertisement { id, .. } => id,
                _ => continue,
            };
            let Ok(p) = central.peripheral(&id).await else { continue };
            let Ok(Some(props)) = p.properties().await else { continue };
            let by_service = props.services.contains(&NUS_SERVICE);
            let by_name = props.local_name.as_deref() == Some(RING_NAME);
            if by_service || by_name {
                found = Some(p);
            }
        }
        let _ = central.stop_scan().await;
        let peripheral = found?;

        tokio::time::timeout(Duration::from_secs(6), peripheral.connect())
            .await
            .ok()?
            .ok()?;
        peripheral.discover_services().await.ok()?;
        let chars = peripheral.characteristics();
        let rx = chars.iter().find(|c| c.uuid == NUS_RX)?.clone();
        let tx = chars.iter().find(|c| c.uuid == NUS_TX)?.clone();
        peripheral.subscribe(&tx).await.ok()?;
        let mut notifications = peripheral.notifications().await.ok()?;

        let (send, lines) = mpsc::channel::<String>();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_task = alive.clone();
        // Reply lines end in '\n' and may be split across notifications
        // (STATE is ~170 bytes); reassemble here so the worker only ever
        // sees whole lines.
        tauri::async_runtime::spawn(async move {
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
        block_on(
            self.peripheral
                .write(&self.rx, format!("{line}\n").as_bytes(), WriteType::WithoutResponse),
        )
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
        match block_on(self.peripheral.is_connected()) {
            Ok(true) => Ok(out),
            _ => Err("ble disconnected".into()),
        }
    }
}

impl Drop for BleLink {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        let _ = block_on(self.peripheral.disconnect());
    }
}
