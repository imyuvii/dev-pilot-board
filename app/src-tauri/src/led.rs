//! Optional LED ring output for the dashboard.
//!
//! The ring (a 16-LED WS2812B on an ESP32, see the led-iot project) speaks one
//! newline-terminated line protocol over BOTH transports: USB serial at 115200
//! baud, or a plain WebSocket on port 81. This module is just another client of
//! that protocol — it never learns anything about events or sources. The
//! frontend decides what the ring should look like and pushes a `LedState`
//! down; everything here is transport plumbing.
//!
//! Hard requirement: the ring is optional. No board, no network, no serial
//! permission, a port held by another app — every one of those paths ends in a
//! quiet retry. Nothing here is allowed to surface an error to the user or to
//! affect sounds, banners or the dashboard.
//!
//! Transport preference is WiFi, then Bluetooth LE, then USB. Only one process
//! can hold a USB serial port, and this app runs all day, so squatting on
//! /dev/cu.usbserial-* would lock out LED Lab, the Arduino IDE and
//! `arduino-cli upload`. Over WebSocket several clients coexist, so when WiFi
//! shows up we drop serial and move. Bluetooth (see `ble.rs`) needs no network
//! and no setup on the ring, but is one Mac at a time.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tungstenite::{Message, WebSocket};

const WS_PORT: u16 = 81;
const BAUD: u32 = 115_200;
/// Cap matching LED Lab's own limit: the firmware budgets 5V/700mA for the ring
/// plus the WiFi radio out of one USB supply, so full brightness browns out.
const MAX_BRIGHT: u8 = 160;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1200);
/// One loop turn blocks about this long draining inbound frames, which doubles
/// as the poll interval for target-state changes.
const DRAIN_WINDOW: Duration = Duration::from_millis(120);
/// In "auto" mode, how often to re-check WiFi while sitting on serial.
const WIFI_RECHECK: Duration = Duration::from_secs(15);
/// How often to ask the ring what it is actually showing, so drift self-heals.
const RESYNC_PROBE: Duration = Duration::from_secs(5);
/// The firmware answers every command with a STATE line, so right after an
/// emit (up to five commands) the replies show intermediate states. Judging
/// drift from those re-sends everything and starts the cycle again; ignore
/// STATE lines for this long after we wrote something.
const SETTLE: Duration = Duration::from_millis(600);
/// Three reconnects inside this window is treated as the ring rebooting.
const BROWNOUT_WINDOW: Duration = Duration::from_secs(90);
/// A link this stable earns full brightness back.
const BROWNOUT_RECOVER: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq)]
pub struct LedState {
    /// "auto" | "wifi" | "ble" | "usb"
    pub transport: String,
    /// mDNS name or IP of the ring, e.g. "ledring.local"
    pub host: String,
    /// One of the firmware's effect names. An unknown name makes the firmware
    /// answer `ERR unknown mode` and keep its current effect, so a typo here
    /// degrades to "ring doesn't change", never to a broken ring.
    pub pattern: String,
    pub color: String,
    pub color2: String,
    pub speed: u8,
    pub bright: u8,
    /// Position on the LADDER (0 = most urgent); `OFF_RANK` when there is
    /// nothing to show. Lets us judge another client's state against ours.
    pub rank: usize,
    /// Every rung's configured look, so a STATE reply can be mapped back to a
    /// rung — that is how we recognise another Mac's status on a shared ring.
    pub looks: Vec<Look>,
}

const OFF_RANK: usize = usize::MAX;

impl LedState {
    fn is_off(&self) -> bool {
        self.rank == OFF_RANK
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub rank: usize,
    pub pattern: String,
    pub color: String,
}

/// What the ring is showing, judged on our ladder.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RingSeen {
    Off,
    /// A rung we recognise by pattern + colour — almost certainly another
    /// Dev Pilot Board on the same network.
    Rung(usize),
    /// Firmware defaults after a reboot, LED Lab, a serial console…
    Foreign,
}

#[derive(Clone, Default, serde::Serialize)]
pub struct LedStatus {
    pub connected: bool,
    /// "wifi" | "usb" | "" when disconnected
    pub transport: String,
    /// Short human-readable line for the settings panel.
    pub detail: String,
}

struct Shared {
    status: Mutex<LedStatus>,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static CLEARED: AtomicBool = AtomicBool::new(false);

fn shared() -> &'static Arc<Shared> {
    SHARED.get_or_init(|| {
        Arc::new(Shared {
            status: Mutex::new(LedStatus::default()),
        })
    })
}

#[tauri::command]
pub fn led_status() -> LedStatus {
    shared()
        .status
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

/// Blank the ring on the way out, so quitting the app doesn't leave a stale
/// colour glowing on the desk. Waits briefly for the worker to confirm.
pub fn shutdown() {
    if !SHUTDOWN.swap(true, Ordering::SeqCst) {
        let deadline = Instant::now() + Duration::from_millis(800);
        while !CLEARED.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Diagnostics for the LED path, off unless DPB_LED_DEBUG is set. The ring
/// failing is never user-visible, so without this there is nothing to look at.
fn dbg(msg: &str) {
    if std::env::var_os("DPB_LED_DEBUG").is_some() {
        eprintln!("[led] {msg}");
    }
}

fn set_status(connected: bool, transport: &str, detail: &str) {
    if let Ok(mut s) = shared().status.lock() {
        s.connected = connected;
        s.transport = transport.to_string();
        s.detail = detail.to_string();
    }
}

// ------------------------------------------------------------- what to show
//
// This lives in Rust, not in the Vue frontend, for one hard reason: Dev Pilot
// Board is a menu-bar app whose window is hidden nearly all the time, and WebKit
// suspends timers in a hidden webview. Driving the ring from the frontend
// therefore worked for one tick after launch and then silently stopped.
//
// KEEP IN SYNC with App.vue: `STATUS_MAP`, the `STATUS_META` ranking and
// `STALE_MS` are duplicated here. The tests at the bottom of this file pin the
// ladder; if you change the dashboard's statuses, change both.

const STALE_SECS: i64 = 12 * 60 * 60;
/// How long a one-off event (failed tool call, task finished…) holds the ring
/// before it falls back to the session's standing status.
const TRANSIENT_HOLD_SECS: i64 = 30;
/// Plenty for the app's own 2000-line log cap, and bounds the per-second read.
const TAIL_BYTES: u64 = 256 * 1024;

/// One rung of the ladder. Colour, pattern and on/off are user-configurable
/// per event under `led.events.<event>` — the same shape as sound and banner
/// — while speed, dim scale and urgency order stay fixed.
struct Rung {
    event: &'static str,
    enabled: bool,
    color: &'static str,
    pattern: &'static str,
    speed: u8,
    /// Brightness multiplier — resting states sit lower than alerts.
    scale: f32,
    /// A transient rung lights up for TRANSIENT_HOLD_SECS after its event and
    /// then yields; a standing rung reflects a session's current status.
    transient: bool,
}

/// Most urgent first. Same order the tray glyph uses, with the one-off events
/// slotted between "needs you" and "busy". KEEP IN SYNC with `LED_EVENTS` in
/// src/led.ts (labels + defaults) and `STATUS_MAP` in App.vue (which events
/// count as which standing status).
///
/// Defaults mirror the author's day-to-day setup: a dim ring that shows blue
/// while an agent works, chases magenta on a question, flickers red on a
/// failure and breathes green for 30s when a response finishes. `waiting` and
/// `task-done` are off because with `working` lit the ring is already busy;
/// they stay available in Settings. Question/waiting hold until answered
/// because an unanswered notification is still a notification.
const LADDER: &[Rung] = &[
    Rung { event: "question",      enabled: true,  color: "ff00aa", pattern: "chase", speed: 75, scale: 1.0,  transient: false },
    Rung { event: "waiting",       enabled: false,  color: "0061ff", pattern: "chase", speed: 45, scale: 1.0,  transient: false },
    Rung { event: "failure",       enabled: true,  color: "ff2020", pattern: "fire",   speed: 50, scale: 1.0,  transient: true },
    Rung { event: "task-done",     enabled: false,  color: "00d8ff", pattern: "juggle", speed: 60, scale: 1.0,  transient: true },
    Rung { event: "compact",       enabled: false, color: "9b59b6", pattern: "breathe", speed: 40, scale: 1.0,  transient: true },
    Rung { event: "session-start", enabled: false, color: "ffffff", pattern: "chase",   speed: 70, scale: 1.0,  transient: true },
    // `working` is a silent status event in notify.sh (no sound/banner); on the
    // ring it is the "agent is busy" idle colour.
    Rung { event: "working",       enabled: true, color: "1e90ff", pattern: "chase",   speed: 65, scale: 1.0,  transient: false },
    Rung { event: "stop",          enabled: true,  color: "4f7a28", pattern: "breathe",   speed: 30, scale: 1.0,  transient: true },
];

fn rung(event: &str) -> Option<&'static Rung> {
    LADDER.iter().find(|r| r.event == event)
}

#[derive(Clone, Debug, PartialEq)]
struct EventLook {
    enabled: bool,
    color: String,
    pattern: String,
}

struct Cfg {
    enabled: bool,
    transport: String,
    host: String,
    brightness: u8,
    dim_in_quiet_hours: bool,
    events: HashMap<String, EventLook>,
    quiet_enabled: bool,
    quiet_start: String,
    quiet_end: String,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            // Opt-in: off means we never resolve mDNS or open a serial port.
            enabled: false,
            transport: "auto".into(),
            host: "ledring.local".into(),
            brightness: 5,
            dim_in_quiet_hours: true,
            events: LADDER
                .iter()
                .map(|r| {
                    (
                        r.event.to_string(),
                        EventLook {
                            enabled: r.enabled,
                            color: r.color.to_string(),
                            pattern: r.pattern.to_string(),
                        },
                    )
                })
                .collect(),
            quiet_enabled: false,
            quiet_start: "22:00".into(),
            quiet_end: "08:00".into(),
        }
    }
}

fn home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

/// The colour picker hands us "#rrggbb"; the firmware wants "rrggbb". Anything
/// that isn't six hex digits is ignored so a bad value can't wedge the ring.
fn clean_color(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('#').to_ascii_lowercase();
    (s.len() == 6 && s.chars().all(|c| c.is_ascii_hexdigit())).then_some(s)
}

/// Tolerant on purpose: a missing, partial or legacy config yields defaults
/// rather than disabling the feature in some confusing half-state.
fn read_cfg() -> Cfg {
    let mut cfg = Cfg::default();
    let Ok(text) = std::fs::read_to_string(home().join(".claude/notify-config.json")) else {
        return cfg;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return cfg;
    };
    if let Some(led) = v.get("led") {
        if let Some(b) = led.get("enabled").and_then(|x| x.as_bool()) {
            cfg.enabled = b;
        }
        if let Some(s) = led.get("transport").and_then(|x| x.as_str()) {
            cfg.transport = s.to_string();
        }
        if let Some(s) = led.get("host").and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                cfg.host = s.trim().to_string();
            }
        }
        if let Some(n) = led.get("brightness").and_then(|x| x.as_u64()) {
            cfg.brightness = n.min(MAX_BRIGHT as u64) as u8;
        }
        if let Some(b) = led.get("dim_in_quiet_hours").and_then(|x| x.as_bool()) {
            cfg.dim_in_quiet_hours = b;
        }
        if let Some(evs) = led.get("events").and_then(|x| x.as_object()) {
            for (name, look) in cfg.events.iter_mut() {
                let Some(e) = evs.get(name) else { continue };
                if let Some(b) = e.get("enabled").and_then(|x| x.as_bool()) {
                    look.enabled = b;
                }
                if let Some(c) = e.get("color").and_then(|x| x.as_str()).and_then(clean_color) {
                    look.color = c;
                }
                if let Some(p) = e.get("pattern").and_then(|x| x.as_str()) {
                    let p = p.trim();
                    if !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()) {
                        look.pattern = p.to_string();
                    }
                }
            }
        }
    }
    if let Some(q) = v.get("quiet_hours") {
        cfg.quiet_enabled = q.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false);
        if let Some(s) = q.get("start").and_then(|x| x.as_str()) {
            cfg.quiet_start = s.to_string();
        }
        if let Some(s) = q.get("end").and_then(|x| x.as_str()) {
            cfg.quiet_end = s.to_string();
        }
    }
    cfg
}

fn hhmm_to_mins(v: &str) -> i32 {
    let mut it = v.split(':');
    let h: i32 = it.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let m: i32 = it.next().unwrap_or("0").trim().parse().unwrap_or(0);
    h * 60 + m
}

/// Mirrors the quiet-hours window in notify.sh, overnight ranges included.
fn in_quiet_hours(cfg: &Cfg, now_mins: i32) -> bool {
    if !cfg.quiet_enabled {
        return false;
    }
    let s = hhmm_to_mins(&cfg.quiet_start);
    let e = hhmm_to_mins(&cfg.quiet_end);
    if s <= e {
        now_mins >= s && now_mins < e
    } else {
        now_mins >= s || now_mins < e
    }
}

/// `notify.sh` appends one JSON object per line and trims to 1000 when it grows
/// past 2000, so reading the tail is enough and keeps this cheap once a second.
fn read_events() -> Vec<(i64, String, String)> {
    let mut out = Vec::new();
    let Ok(mut f) = std::fs::File::open(home().join(".claude/notify-state.jsonl")) else {
        return out;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(TAIL_BYTES);
    if from > 0 && f.seek(SeekFrom::Start(from)).is_err() {
        return out;
    }
    let mut text = String::new();
    if f.read_to_string(&mut text).is_err() {
        return out;
    }
    for (i, line) in text.lines().enumerate() {
        // A mid-line seek leaves a partial first line; skip it.
        if i == 0 && from > 0 {
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let Some(ts) = chrono::DateTime::parse_from_rfc3339(&s("ts")).ok() else {
            continue;
        };
        let key = [s("session"), s("cwd"), s("project")]
            .into_iter()
            .find(|k| !k.is_empty())
            .unwrap_or_default();
        out.push((ts.timestamp(), s("event"), key));
    }
    out
}

/// event -> a session's standing status, matching App.vue's STATUS_MAP
/// ("done" there is the `stop` rung here).
fn standing_of(event: &str) -> Option<&'static str> {
    match event {
        "question" => Some("question"),
        "waiting" => Some("waiting"),
        "working" | "compact" | "failure" | "task-done" => Some("working"),
        "stop" | "session-start" => Some("stop"),
        _ => None,
    }
}

/// Walk the ladder top-down and return the first rung that is both enabled
/// in config and currently true. `None` = nothing to show, ring off.
fn pick_event(events: &[(i64, String, String)], now: i64, cfg: &Cfg) -> Option<&'static str> {
    let mut sessions: HashMap<String, (i64, &'static str)> = HashMap::new();
    let mut flashes: Vec<&str> = Vec::new();
    for (ts, event, key) in events {
        if event == "session-end" {
            sessions.remove(key);
            continue;
        }
        if now - *ts <= TRANSIENT_HOLD_SECS
            && rung(event).is_some_and(|r| r.transient)
        {
            flashes.push(event.as_str());
        }
        if let Some(st) = standing_of(event) {
            sessions.insert(key.clone(), (*ts, st));
        }
    }
    sessions.retain(|_, (ts, _)| now - *ts < STALE_SECS);
    if sessions.is_empty() {
        return None;
    }
    let standing = |st: &str| sessions.values().any(|(_, s)| *s == st);
    LADDER
        .iter()
        .filter(|r| cfg.events.get(r.event).is_some_and(|e| e.enabled))
        .find(|r| {
            if r.transient {
                flashes.contains(&r.event)
            } else {
                standing(r.event)
            }
        })
        .map(|r| r.event)
}

/// The whole decision: config + event log -> one colour and one pattern.
/// `None` means the feature is off and the ring should be released.
fn compute_target() -> Option<LedState> {
    let cfg = read_cfg();
    if !cfg.enabled {
        return None;
    }
    let now = chrono::Utc::now().timestamp();
    let picked = pick_event(&read_events(), now, &cfg);
    let rank = picked
        .and_then(|e| LADDER.iter().position(|r| r.event == e))
        .unwrap_or(OFF_RANK);

    let (pattern, color, speed, scale) = match picked.and_then(rung) {
        Some(r) => {
            let look = cfg.events.get(r.event);
            (
                look.map(|l| l.pattern.clone()).unwrap_or_else(|| r.pattern.to_string()),
                look.map(|l| l.color.clone()).unwrap_or_else(|| r.color.to_string()),
                r.speed,
                r.scale,
            )
        }
        None => ("off".to_string(), "000000".to_string(), 50, 1.0),
    };
    let looks = LADDER
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let look = cfg.events.get(r.event);
            Look {
                rank: i,
                pattern: look.map(|l| l.pattern.clone()).unwrap_or_else(|| r.pattern.to_string()),
                color: look.map(|l| l.color.clone()).unwrap_or_else(|| r.color.to_string()),
            }
        })
        .collect();

    let mut bright = (cfg.brightness.min(MAX_BRIGHT) as f32 * scale).round() as u8;
    // Quiet hours dim rather than blank: a dark ring is indistinguishable from
    // an unplugged one.
    if cfg.dim_in_quiet_hours {
        let local = chrono::Local::now();
        let mins = chrono::Timelike::hour(&local) as i32 * 60
            + chrono::Timelike::minute(&local) as i32;
        if in_quiet_hours(&cfg, mins) {
            bright = bright.min(10);
        }
    }

    Some(LedState {
        transport: cfg.transport,
        host: cfg.host,
        pattern,
        color,
        color2: "000000".to_string(),
        speed,
        bright,
        rank,
        looks,
    })
}

// ------------------------------------------------------------------ transport

enum Conn {
    Ws(WebSocket<TcpStream>),
    Serial(Box<dyn serialport::SerialPort>),
    Ble(crate::ble::BleLink),
}

impl Conn {
    fn label(&self) -> &'static str {
        match self {
            Conn::Ws(_) => "wifi",
            Conn::Serial(_) => "usb",
            Conn::Ble(_) => "ble",
        }
    }

    fn is_serial(&self) -> bool {
        matches!(self, Conn::Serial(_))
    }

    fn write_line(&mut self, line: &str) -> Result<(), String> {
        match self {
            Conn::Ws(ws) => ws
                .send(Message::Text(line.to_string()))
                .map_err(|e| e.to_string()),
            Conn::Serial(port) => port
                .write_all(format!("{line}\n").as_bytes())
                .map_err(|e| e.to_string()),
            Conn::Ble(link) => link.write_line(line),
        }
    }

    /// The firmware streams its pixel buffer back at 20fps (`F <96 hex>`) to
    /// whoever is listening. We don't want it, but we must read it or the
    /// socket/serial buffer fills and writes eventually stall. Deliberately
    /// *not* disabling it with `STREAM 0`: that flag is global firmware state
    /// shared with every other client, so it would kill LED Lab's live mirror.
    ///
    /// Returns the non-frame reply lines, which is how `STATE {json}` gets back
    /// to the resync check.
    fn drain(&mut self) -> Result<Vec<String>, String> {
        let deadline = Instant::now() + DRAIN_WINDOW;
        let mut lines = Vec::new();
        match self {
            Conn::Ble(link) => link.drain(DRAIN_WINDOW),
            Conn::Ws(ws) => {
                while Instant::now() < deadline {
                    match ws.read() {
                        Ok(Message::Text(t)) => {
                            if !t.starts_with("F ") {
                                lines.push(t);
                            }
                        }
                        Ok(_) => continue,
                        Err(tungstenite::Error::Io(e))
                            if e.kind() == ErrorKind::WouldBlock
                                || e.kind() == ErrorKind::TimedOut =>
                        {
                            break
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                }
                // Flushes any Pong tungstenite queued while reading Pings.
                match ws.flush() {
                    Ok(()) => Ok(lines),
                    Err(tungstenite::Error::Io(e))
                        if e.kind() == ErrorKind::WouldBlock
                            || e.kind() == ErrorKind::TimedOut =>
                    {
                        Ok(lines)
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
            Conn::Serial(port) => {
                let mut buf = [0u8; 512];
                let mut text = String::new();
                while Instant::now() < deadline {
                    match port.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
                        Err(e)
                            if e.kind() == ErrorKind::TimedOut
                                || e.kind() == ErrorKind::WouldBlock =>
                        {
                            break
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                }
                // Partial trailing lines are simply dropped; the resync probe
                // repeats, so a truncated STATE just gets picked up next time.
                for l in text.lines() {
                    let l = l.trim();
                    if !l.is_empty() && !l.starts_with("F ") {
                        lines.push(l.to_string());
                    }
                }
                Ok(lines)
            }
        }
    }
}

/// Does the ring's own reported `STATE {json}` match what we asked for?
///
/// Why bother asking: this is a status light, so a stale colour is a lie. The
/// ring can drift out of sync for reasons no write error reveals — it reboots
/// and comes back on defaults, a command is lost, or another client (LED Lab,
/// a serial console) changes the mode underneath us. Comparing beats blindly
/// re-sending, which would restart the animation every time.
fn state_matches(line: &str, want: &LedState) -> Option<bool> {
    let json = line.strip_prefix("STATE ")?;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    let n = |k: &str| v.get(k).and_then(|x| x.as_u64());
    Some(
        s("mode").as_deref() == Some(want.pattern.as_str())
            && n("bright") == Some(want.bright.min(MAX_BRIGHT) as u64)
            && n("speed") == Some(want.speed.clamp(1, 100) as u64)
            && s("color").as_deref() == Some(want.color.as_str())
            && s("color2").as_deref() == Some(want.color2.as_str()),
    )
}

// ----------------------------------------------------------------- discovery
//
// Finding the ring must not depend on the Mac's own resolver. `ledring.local`
// goes through mDNSResponder, and a VPN client that hijacks DNS, a firewall
// set to "block all incoming connections" or a switch that filters multicast
// all break it silently: the ring is reachable, yet `ping ledring.local`
// says "unknown host". Seen on a second Mac on the very same subnet. Ladder,
// cheapest first:
//   1. the configured host is already an IP
//   2. the system resolver (getaddrinfo)
//   3. our own mDNS query from an ephemeral port — RFC 6762 §6.7 "legacy
//      unicast": the ring answers straight back to us, no multicast receive
//      and no mDNSResponder involved
//   4. the last IP that worked, if it still answers `STATE?` like a ring
//   5. sweep the local /24 for a port-81 listener that answers `STATE?`
// Whatever 3–5 find is remembered in ~/.claude/notify-led-cache.json so the
// next launch skips the sweep. Same rules as everything else here: every
// failure is a quiet `None`, never an error the user sees.

const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const MDNS_PORT: u16 = 5353;
const MDNS_WAIT: Duration = Duration::from_millis(800);
/// A TCP connect to a silent address burns the whole timeout; keep it short
/// and run many in parallel so a /24 sweep finishes in a few seconds. 300ms
/// missed a ring that was busy serving another client, hence 500 and a
/// second pass.
const SCAN_TIMEOUT: Duration = Duration::from_millis(500);
const SCAN_PASSES: usize = 2;
const SCAN_THREADS: usize = 32;
/// Sweeping the LAN is the noisy last resort — never more often than this.
const SCAN_MIN_INTERVAL: Duration = Duration::from_secs(60);
/// How long a candidate gets to prove it is a ring by answering `STATE?`.
const PROBE_WAIT: Duration = Duration::from_millis(800);

static LAST_SCAN: Mutex<Option<Instant>> = Mutex::new(None);

fn cache_path() -> std::path::PathBuf {
    home().join(".claude/notify-led-cache.json")
}

/// Build a DNS A query for `name` as mDNS wants it: id 0, no flags, one
/// question, class IN. Returns `None` for a name that is not a `.local` label
/// sequence — unicast DNS names belong to the system resolver.
fn mdns_packet(name: &str) -> Option<Vec<u8>> {
    let name = name.trim_end_matches('.');
    if !name.ends_with(".local") {
        return None;
    }
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0, 1, 0, 1]); // QTYPE A, QCLASS IN
    Some(q)
}

/// Read a (possibly compressed) DNS name at `i` into `out`; returns the index
/// just past it in the *original* stream. Compression pointers are followed
/// with a hop limit so a malicious packet cannot loop us.
fn dns_name(buf: &[u8], mut i: usize, out: &mut String, hops: u8) -> Option<usize> {
    if hops > 8 {
        return None;
    }
    loop {
        let len = *buf.get(i)? as usize;
        if len == 0 {
            return Some(i + 1);
        }
        if len & 0xC0 == 0xC0 {
            let ptr = ((len & 0x3F) << 8) | *buf.get(i + 1)? as usize;
            dns_name(buf, ptr, out, hops + 1)?;
            return Some(i + 2);
        }
        let label = buf.get(i + 1..i + 1 + len)?;
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(&String::from_utf8_lossy(label));
        i += 1 + len;
    }
}

/// Pull the first A record for `want` out of a DNS/mDNS response.
fn mdns_answer(buf: &[u8], want: &str) -> Option<Ipv4Addr> {
    if buf.len() < 12 || buf[2] & 0x80 == 0 {
        return None; // not a response
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let want = want.trim_end_matches('.');
    let mut i = 12;
    for _ in 0..qd {
        i = dns_name(buf, i, &mut String::new(), 0)? + 4;
    }
    for _ in 0..an {
        let mut name = String::new();
        i = dns_name(buf, i, &mut name, 0)?;
        let ty = u16::from_be_bytes([*buf.get(i)?, *buf.get(i + 1)?]);
        let rdlen = u16::from_be_bytes([*buf.get(i + 8)?, *buf.get(i + 9)?]) as usize;
        let rd = buf.get(i + 10..i + 10 + rdlen)?;
        if ty == 1 && rdlen == 4 && name.eq_ignore_ascii_case(want) {
            return Some(Ipv4Addr::new(rd[0], rd[1], rd[2], rd[3]));
        }
        i += 10 + rdlen;
    }
    None
}

fn mdns_query(host: &str) -> Option<Ipv4Addr> {
    let q = mdns_packet(host)?;
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    sock.set_read_timeout(Some(MDNS_WAIT)).ok()?;
    sock.send_to(&q, (MDNS_GROUP, MDNS_PORT)).ok()?;
    let deadline = Instant::now() + MDNS_WAIT;
    let mut buf = [0u8; 1500];
    while Instant::now() < deadline {
        let Ok((n, _)) = sock.recv_from(&mut buf) else { break };
        if let Some(ip) = mdns_answer(&buf[..n], host) {
            return Some(ip);
        }
    }
    None
}

fn cached_ip(host: &str) -> Option<Ipv4Addr> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    if v.get("host")?.as_str()? != host {
        return None;
    }
    v.get("ip")?.as_str()?.parse().ok()
}

fn remember_ip(host: &str, ip: Ipv4Addr) {
    let v = serde_json::json!({ "host": host, "ip": ip.to_string() });
    let _ = std::fs::write(cache_path(), v.to_string());
}

/// The interface the default route leaves by — that is the LAN the ring is
/// on. A UDP connect sends nothing; it only asks the kernel to pick a source.
fn local_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(1, 1, 1, 1), 53)).ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if ip.is_private() => Some(ip),
        _ => None,
    }
}

fn ws_open(addr: SocketAddr, host: &str) -> Option<WebSocket<TcpStream>> {
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    stream.set_nodelay(true).ok();
    // Handshake on a blocking stream: a read timeout here would surface as a
    // spurious Interrupted handshake error.
    let (ws, _resp) = tungstenite::client::client(format!("ws://{host}:{WS_PORT}/"), stream).ok()?;
    Some(ws)
}

/// A port-81 listener is only a ring if it talks the ring's protocol.
fn is_ring(ip: Ipv4Addr) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(ip), WS_PORT);
    let Some(mut ws) = ws_open(addr, &ip.to_string()) else { return false };
    if ws.get_ref().set_read_timeout(Some(PROBE_WAIT)).is_err() {
        return false;
    }
    if ws.send(Message::Text("STATE?".into())).is_err() {
        return false;
    }
    // The firmware may greet first; allow a few frames before giving up.
    for _ in 0..4 {
        match ws.read() {
            Ok(Message::Text(t)) if t.starts_with("STATE ") => return true,
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
    false
}

fn scan_subnet(local: Ipv4Addr) -> Option<Ipv4Addr> {
    (0..SCAN_PASSES).find_map(|_| scan_subnet_once(local).0)
}

/// One pass: (the first port-81 listener that answers like a ring, every
/// port-81 listener seen). The second half is for the diagnostics panel.
fn scan_subnet_once(local: Ipv4Addr) -> (Option<Ipv4Addr>, Vec<Ipv4Addr>) {
    let [a, b, c, me] = local.octets();
    let hosts: Vec<Ipv4Addr> = (1..=254u8)
        .filter(|&d| d != me)
        .map(|d| Ipv4Addr::new(a, b, c, d))
        .collect();
    let open: Mutex<Vec<Ipv4Addr>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for chunk in hosts.chunks(hosts.len().div_ceil(SCAN_THREADS)) {
            let open = &open;
            s.spawn(move || {
                for &ip in chunk {
                    let addr = SocketAddr::new(IpAddr::V4(ip), WS_PORT);
                    if TcpStream::connect_timeout(&addr, SCAN_TIMEOUT).is_ok() {
                        if let Ok(mut o) = open.lock() {
                            o.push(ip);
                        }
                    }
                }
            });
        }
    });
    let mut open = open.into_inner().unwrap_or_default();
    open.sort();
    dbg(&format!("scan {a}.{b}.{c}.0/24: port 81 open on {open:?}"));
    let hit = open.iter().copied().find(|&ip| is_ring(ip));
    (hit, open)
}

/// The default gateway, from the routing table. Reaching it is the cheapest
/// proof that this app is allowed to talk to the LAN at all.
fn gateway() -> Option<Ipv4Addr> {
    let out = std::process::Command::new("/sbin/route").args(["-n", "get", "default"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("gateway:"))
        .and_then(|g| g.trim().parse().ok())
}

/// What a failed LAN connect means, in words. macOS answers "No route to
/// host" (EHOSTUNREACH) for *every* local address when Local Network access
/// is denied for the app — that is the signature we want to name.
enum LanVerdict {
    Reachable,
    Blocked,
    Silent,
}

fn lan_probe(ip: Ipv4Addr, port: u16) -> LanVerdict {
    let addr = SocketAddr::new(IpAddr::V4(ip), port);
    match TcpStream::connect_timeout(&addr, Duration::from_millis(1000)) {
        Ok(_) => LanVerdict::Reachable,
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => LanVerdict::Reachable,
        // 65 EHOSTUNREACH, 51 ENETUNREACH, 50 ENETDOWN on macOS
        Err(e) if matches!(e.raw_os_error(), Some(65) | Some(51) | Some(50)) => LanVerdict::Blocked,
        Err(_) => LanVerdict::Silent,
    }
}

/// Turn the configured host into an address the ring actually answers on.
fn resolve_ring(host: &str) -> Option<Ipv4Addr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        };
    }
    if let Some(ip) = (host, WS_PORT)
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.find_map(|a| match a.ip() {
            IpAddr::V4(v4) => Some(v4),
            _ => None,
        }))
    {
        return Some(ip);
    }
    if let Some(ip) = mdns_query(host) {
        dbg(&format!("system resolver failed, own mDNS query found {host} at {ip}"));
        remember_ip(host, ip);
        return Some(ip);
    }
    if let Some(ip) = cached_ip(host) {
        if is_ring(ip) {
            dbg(&format!("using remembered address {ip} for {host}"));
            return Some(ip);
        }
    }
    let due = LAST_SCAN
        .lock()
        .ok()
        .map(|t| t.map_or(true, |t| t.elapsed() >= SCAN_MIN_INTERVAL))
        .unwrap_or(false);
    if due {
        if let Ok(mut t) = LAST_SCAN.lock() {
            *t = Some(Instant::now());
        }
        if let Some(ip) = local_ipv4().and_then(scan_subnet) {
            dbg(&format!("subnet sweep found a ring at {ip}, remembering it for {host}"));
            remember_ip(host, ip);
            return Some(ip);
        }
    }
    None
}

/// One rung of the discovery ladder as the Settings panel shows it.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct DiscoveryStep {
    pub label: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct DiscoveryReport {
    pub version: String,
    pub host: String,
    pub local_ip: String,
    pub steps: Vec<DiscoveryStep>,
    /// Address the ring answered on, if any rung found one.
    pub found: String,
    /// What to try next when nothing was found. Empty when found.
    pub hint: String,
}

/// Run every rung of the ladder and say what each one saw — the on-screen
/// version of `DPB_LED_DEBUG=1`, so a user on another Mac never needs a
/// terminal to learn why the ring is dark. Unlike `resolve_ring` it does not
/// stop at the first hit and ignores the sweep rate limit (it is user-driven).
pub fn discover(host: &str) -> DiscoveryReport {
    let mut rep = DiscoveryReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        host: host.to_string(),
        ..Default::default()
    };
    let mut step = |label: &str, ok: bool, detail: String| {
        rep.steps.push(DiscoveryStep { label: label.into(), ok, detail });
    };
    let mut found: Option<Ipv4Addr> = None;

    let local = local_ipv4();
    rep.local_ip = local.map(|ip| ip.to_string()).unwrap_or_default();
    step(
        "This Mac's network",
        local.is_some(),
        match local {
            Some(ip) => format!("On {ip}"),
            None => "No LAN address — WiFi off, or every route goes through a VPN".into(),
        },
    );

    let gw = gateway();
    let mut lan = LanVerdict::Silent;
    match (local, gw) {
        (Some(_), Some(g)) => {
            lan = lan_probe(g, 80);
            let (ok, detail) = match lan {
                LanVerdict::Reachable => (true, format!("router {g} answers — this app may use the local network")),
                LanVerdict::Blocked => (false, format!("router {g}: \"No route to host\" — macOS is blocking this app's local-network access")),
                LanVerdict::Silent => (false, format!("router {g} did not answer on port 80 (not conclusive)")),
            };
            step("Local network access", ok, detail);
        }
        (Some(_), None) => step("Local network access", false, "no default route found".into()),
        (None, _) => {}
    }

    if let Ok(IpAddr::V4(ip)) = host.parse::<IpAddr>() {
        let ok = is_ring(ip);
        step("Configured address", ok, if ok { format!("{ip} answers like a ring") } else { format!("{ip} does not answer on port {WS_PORT}") });
        if ok {
            found = Some(ip);
        }
    } else {
        let sys = (host, WS_PORT).to_socket_addrs().ok().and_then(|mut it| {
            it.find_map(|a| match a.ip() {
                IpAddr::V4(v4) => Some(v4),
                _ => None,
            })
        });
        step(
            "macOS name lookup",
            sys.is_some(),
            match sys {
                Some(ip) => format!("{host} → {ip}"),
                None => format!("{host} does not resolve on this Mac (VPN or firewall blocking mDNS)"),
            },
        );
        if let Some(ip) = sys {
            found.get_or_insert(ip);
        }

        let own = mdns_query(host);
        step(
            "App's own mDNS query",
            own.is_some(),
            match own {
                Some(ip) => format!("ring answered from {ip}"),
                None => "no answer in 0.8s".into(),
            },
        );
        if let Some(ip) = own {
            found.get_or_insert(ip);
        }

        match cached_ip(host) {
            Some(ip) => {
                let ok = is_ring(ip);
                step("Remembered address", ok, if ok { format!("{ip} still answers") } else { format!("{ip} no longer answers") });
                if ok {
                    found.get_or_insert(ip);
                }
            }
            None => step("Remembered address", false, "none saved yet".into()),
        }
    }

    match local {
        Some(l) => {
            if let Ok(mut t) = LAST_SCAN.lock() {
                *t = Some(Instant::now());
            }
            let [a, b, c, _] = l.octets();
            let (mut hit, mut open) = scan_subnet_once(l);
            if hit.is_none() {
                let again = scan_subnet_once(l);
                hit = again.0;
                open.extend(again.1);
                open.sort();
                open.dedup();
            }
            let listeners: Vec<String> = open.iter().map(|ip| ip.to_string()).collect();
            step(
                "Sweep of the local network",
                hit.is_some(),
                match hit {
                    Some(ip) => format!("{a}.{b}.{c}.0/24 — ring found at {ip}"),
                    None if !open.is_empty() => format!(
                        "{a}.{b}.{c}.0/24 — port 81 open on {} but no ring answered STATE? (LED Lab connected, or the ring's client slots are full)",
                        listeners.join(", ")
                    ),
                    None => format!("{a}.{b}.{c}.0/24 — nothing on port 81 at all"),
                },
            );
            if let Some(ip) = hit {
                found.get_or_insert(ip);
            }
        }
        None => step("Sweep of the local network", false, "skipped — no LAN address".into()),
    }

    if let Some(ip) = found {
        remember_ip(host, ip);
        rep.found = ip.to_string();
    } else {
        rep.hint = match (local, lan) {
            (None, _) => "Join the same WiFi as the ring, then try again.".into(),
            (_, LanVerdict::Blocked) => "macOS is blocking this app's local-network access. Open System Settings → \
                 Privacy & Security → Local Network and switch Dev Pilot Board on. If it is not listed, quit and \
                 reopen the app so macOS asks; then press Find my ring again. Plugging the ring in over USB works \
                 without any of this."
                .into(),
            (_, LanVerdict::Reachable) => "This Mac reaches the router but nothing else on the WiFi. Typical causes: \
                 the router isolates clients from each other (AP/client isolation or a guest network), or the ring \
                 is on a different band/VLAN. Plugging the ring in over USB works without any of this."
                .into(),
            (_, LanVerdict::Silent) => "The ring did not answer anywhere on this network. Check that it is powered \
                 and on this WiFi (its light shows rainbow while unconfigured). If it works from another Mac, \
                 check System Settings → Privacy & Security → Local Network for Dev Pilot Board. Plugging the ring \
                 in over USB works without any of this."
                .into(),
        };
    }
    rep
}

#[tauri::command]
pub async fn led_discover(host: String) -> DiscoveryReport {
    let host = if host.trim().is_empty() { Cfg::default().host } else { host.trim().to_string() };
    tauri::async_runtime::spawn_blocking(move || discover(&host))
        .await
        .unwrap_or_default()
}

// ------------------------------------------------------------- shared ring
//
// One ring, several Macs. Each app used to treat any mismatch as drift and
// re-assert its own view every 5s, so an idle laptop forced the ring dark
// while the desktop was mid-task (seen for real). Rules now:
//   * nothing to show → never blank a ring another client lit; only blank
//     what we lit ourselves, or firmware defaults after a reboot
//   * something to show → yield to a recognised status that is as urgent or
//     more urgent than ours; override a dark ring, a less urgent status, or
//     anything foreign (reboot defaults, LED Lab)
// Recognition is by pattern + colour against our own configured looks, so
// two Macs with the same defaults agree; a custom colour on one Mac makes it
// "foreign" to the other and falls back to the old override behaviour.

fn ring_seen(line: &str, looks: &[Look]) -> Option<RingSeen> {
    let json = line.strip_prefix("STATE ")?;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mode = v.get("mode")?.as_str()?;
    if mode == "off" {
        return Some(RingSeen::Off);
    }
    let color = v.get("color").and_then(|x| x.as_str()).unwrap_or("");
    Some(
        looks
            .iter()
            .find(|l| l.pattern == mode && l.color == color)
            .map_or(RingSeen::Foreign, |l| RingSeen::Rung(l.rank)),
    )
}

/// Given what the ring shows (and that it is not our own state), leave it?
fn yields_to(target: &LedState, seen: RingSeen) -> bool {
    match seen {
        RingSeen::Off => false,
        RingSeen::Foreign => false,
        RingSeen::Rung(k) => target.is_off() || k <= target.rank,
    }
}

fn connect_wifi(host: &str) -> Option<Conn> {
    let ip = resolve_ring(host)?;
    let ws = ws_open(SocketAddr::new(IpAddr::V4(ip), WS_PORT), host)?;
    if let Ok(()) = ws.get_ref().set_read_timeout(Some(DRAIN_WINDOW)) {
        if let Ok(mut s) = LAST_WIFI_IP.lock() {
            *s = Some(ip.to_string());
        }
        Some(Conn::Ws(ws))
    } else {
        None
    }
}

static LAST_WIFI_IP: Mutex<Option<String>> = Mutex::new(None);

/// macOS exposes both /dev/tty.* (blocks until carrier detect) and /dev/cu.*
/// (callout, what we want) for the same device. Normalise to cu and dedupe.
fn candidate_ports() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let ports = match serialport::available_ports() {
        Ok(p) => p,
        Err(_) => return out,
    };
    for p in ports {
        let name = p.port_name.replace("/dev/tty.", "/dev/cu.");
        if !(name.contains("usbserial") || name.contains("usbmodem") || name.contains("wchusb")) {
            continue;
        }
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

fn connect_usb() -> Option<Conn> {
    for name in candidate_ports() {
        // ESP32 dev boards wire DTR/RTS to the auto-reset circuit, so a plain
        // open() reboots the board. That is merely annoying on a healthy ring
        // (it comes back in ~3s) but on a marginal USB supply it can keep a
        // boot-looping board from ever recovering, since we probe on a timer.
        let port = serialport::new(&name, BAUD)
            .timeout(Duration::from_millis(250))
            .preserve_dtr_on_open()
            .open();
        let Ok(mut port) = port else { continue };

        // Any USB serial device could be sitting on this port — another dev
        // board, a printer, a radio. Ask before talking: only the ring answers
        // PONG. Without this we'd spray MODE/COLOR at unrelated hardware.
        if port.write_all(b"PING\n").is_err() {
            continue;
        }
        let mut seen = String::new();
        let mut buf = [0u8; 512];
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut ok = false;
        while Instant::now() < deadline {
            match port.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    seen.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if seen.contains("PONG") || seen.contains("ring_control") {
                        ok = true;
                        break;
                    }
                    if seen.len() > 8192 {
                        seen.clear();
                    }
                }
                Err(e) if e.kind() == ErrorKind::TimedOut => continue,
                Err(_) => break,
            }
        }
        if ok {
            return Some(Conn::Serial(port));
        }
    }
    None
}

fn connect_ble() -> Option<Conn> {
    let link = crate::ble::connect()?;
    dbg(&format!("ble: connected to {}", link.id));
    Some(Conn::Ble(link))
}

fn connect(target: &LedState) -> Option<Conn> {
    match target.transport.as_str() {
        "wifi" => connect_wifi(&target.host),
        "ble" => connect_ble(),
        "usb" => connect_usb(),
        // auto: WiFi, then Bluetooth, serial only as the last fallback — see
        // the module note on why this app must not hold the USB port when it
        // doesn't have to.
        _ => connect_wifi(&target.host)
            .or_else(connect_ble)
            .or_else(connect_usb),
    }
}

// -------------------------------------------------------------------- emitter

/// Send only what changed. `MODE` restarts the effect (the firmware zeroes its
/// step counter and blanks the ring), so re-sending it every turn would leave
/// any animation permanently stuck on frame one.
fn emit(conn: &mut Conn, want: &LedState, sent: &Option<LedState>) -> Result<(), String> {
    let bright = want.bright.min(MAX_BRIGHT);
    let speed = want.speed.clamp(1, 100);

    let fresh = sent.is_none();
    let prev = sent.as_ref();
    if fresh || prev != Some(want) {
        dbg(&format!(
            "-> {} color={} bright={} speed={}",
            want.pattern, want.color, bright, speed
        ));
    }

    if fresh || prev.map(|p| p.bright.min(MAX_BRIGHT)) != Some(bright) {
        conn.write_line(&format!("BRIGHT {bright}"))?;
    }
    if fresh || prev.map(|p| p.speed.clamp(1, 100)) != Some(speed) {
        conn.write_line(&format!("SPEED {speed}"))?;
    }
    if fresh || prev.map(|p| p.color.as_str()) != Some(want.color.as_str()) {
        conn.write_line(&format!("COLOR {}", want.color))?;
    }
    if fresh || prev.map(|p| p.color2.as_str()) != Some(want.color2.as_str()) {
        conn.write_line(&format!("COLOR2 {}", want.color2))?;
    }
    // Mode last, so the effect starts with its colours already in place.
    if fresh || prev.map(|p| p.pattern.as_str()) != Some(want.pattern.as_str()) {
        conn.write_line(&format!("MODE {}", want.pattern))?;
    }
    Ok(())
}

/// Settings-panel text, so a ring that is dim because of power says so instead
/// of looking like the brightness slider is broken.
fn brownout_note(label: &str, div: u8) -> String {
    let base = match label {
        "wifi" => match LAST_WIFI_IP.lock().ok().and_then(|s| s.clone()) {
            Some(ip) => format!("Connected over WiFi ({ip})"),
            None => "Connected over WiFi".to_string(),
        },
        "ble" => "Connected over Bluetooth".to_string(),
        _ => "Connected over USB".to_string(),
    };
    if div > 1 {
        format!("{base} — ring keeps resetting, brightness reduced (check its power)")
    } else {
        base.to_string()
    }
}

fn backoff(fails: u32) -> Duration {
    Duration::from_secs((2 * fails.min(15)) as u64).max(Duration::from_secs(2))
}

/// Spawn the LED worker. Cheap when the feature is off: with `enabled: false`
/// it never probes mDNS and never opens a serial port.
pub fn start() {
    let _ = shared();
    std::thread::Builder::new()
        .name("led-ring".into())
        .spawn(run)
        .ok();
}

/// Turning the feature off, or quitting, must leave the ring dark. Just
/// dropping the connection would abandon it mid-colour, still glowing.
fn blank(conn: &mut Option<Conn>) {
    if let Some(c) = conn.as_mut() {
        let _ = c.write_line("CLEAR");
        // CLEAR is answered asynchronously; give the write a moment to leave.
        std::thread::sleep(Duration::from_millis(60));
    }
    *conn = None;
}

fn run() {
    let mut conn: Option<Conn> = None;
    let mut sent: Option<LedState> = None;
    let mut fails: u32 = 0;
    let mut next_try = Instant::now();
    let mut next_wifi_check = Instant::now() + WIFI_RECHECK;
    let mut next_probe = Instant::now();
    let mut cached: Option<LedState> = None;
    // Last STATE reply, judged on our ladder, and whether it was our own state.
    let mut ring: Option<RingSeen> = None;
    let mut ring_ours = false;
    let mut yielding = false;
    let mut settle_until = Instant::now();
    let mut next_compute = Instant::now();
    // Brownout guard, see `brownout_note` below.
    let mut connects: Vec<Instant> = Vec::new();
    let mut brownout_div: u8 = 1;
    let mut stable_since = Instant::now();

    loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            blank(&mut conn);
            CLEARED.store(true, Ordering::SeqCst);
            return;
        }

        // The loop turns every ~120ms draining frames; re-deriving the target
        // from two files that often would be wasteful.
        if Instant::now() >= next_compute {
            next_compute = Instant::now() + Duration::from_millis(1000);
            let fresh = compute_target();
            if fresh != cached {
                dbg(&format!("target <- {fresh:?}"));
                cached = fresh;
            }
        }
        let target = cached.clone();

        // A ring resetting under its own load must not be held there by us.
        let target = target.map(|mut t| {
            if brownout_div > 1 {
                t.bright = (t.bright / brownout_div).max(8);
            }
            t
        });

        let Some(target) = target else {
            // Feature off: blank the ring, then let go of the hardware so
            // nothing else is blocked out of the serial port.
            if conn.is_some() {
                blank(&mut conn);
                sent = None;
                set_status(false, "", "");
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };

        // Sitting on serial in auto mode: keep an eye out for the ring turning
        // up on the network, and hand the USB port back when it does.
        if conn.as_ref().is_some_and(|c| c.is_serial())
            && target.transport == "auto"
            && Instant::now() >= next_wifi_check
        {
            next_wifi_check = Instant::now() + WIFI_RECHECK;
            if let Some(ws) = connect_wifi(&target.host) {
                dbg("wifi is back, releasing the serial port");
                conn = Some(ws);
                sent = None;
                set_status(true, "wifi", &brownout_note("wifi", 1));
            }
        }

        if conn.is_none() {
            if Instant::now() < next_try {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            match connect(&target) {
                Some(c) => {
                    let label = c.label();
                    conn = Some(c);
                    sent = None;
                    fails = 0;
                    next_wifi_check = Instant::now() + WIFI_RECHECK;
                    stable_since = Instant::now();

                    // Repeated reconnects in a short window mean the ring keeps
                    // rebooting, and the most common cause is the LEDs browning
                    // out the board on USB power. Back the brightness off rather
                    // than re-applying the value that just killed it.
                    let now = Instant::now();
                    connects.push(now);
                    connects.retain(|t| now.duration_since(*t) < BROWNOUT_WINDOW);
                    if connects.len() >= 3 && brownout_div < 4 {
                        brownout_div *= 2;
                        connects.clear();
                        dbg(&format!("suspected brownout, brightness /{brownout_div}"));
                    }

                    dbg(&format!("connected over {label}"));
                    set_status(
                        true,
                        label,
                        &brownout_note(label, brownout_div),
                    );
                }
                None => {
                    fails = fails.saturating_add(1);
                    next_try = Instant::now() + backoff(fails);
                    dbg(&format!("no ring found, retry in {:?}", backoff(fails)));
                    set_status(false, "", "No ring found — will keep looking");
                    continue;
                }
            }
        }

        if brownout_div > 1 && conn.is_some() && stable_since.elapsed() > BROWNOUT_RECOVER {
            brownout_div = 1;
            connects.clear();
            stable_since = Instant::now();
            sent = None; // re-apply at full brightness
            dbg("link stable, restoring brightness");
        }

        if let Some(c) = conn.as_mut() {
            let label = c.label();
            // Nothing to show: blank only a ring we lit ourselves (or one on
            // firmware defaults). One another Mac lit — or one we have not
            // heard from yet — is left alone.
            let hold_off = target.is_off()
                && match ring {
                    None | Some(RingSeen::Off) => true,
                    Some(RingSeen::Rung(_)) => !ring_ours,
                    Some(RingSeen::Foreign) => false,
                };
            let mut failed = false;
            if hold_off {
                sent = Some(target.clone());
            } else {
                let changed = sent.as_ref() != Some(&target);
                failed = emit(c, &target, &sent).is_err();
                if !failed {
                    sent = Some(target.clone());
                    if changed {
                        settle_until = Instant::now() + SETTLE;
                    }
                }
            }
            // Ask what the ring is really showing, on a slower cadence than
            // the loop so it costs one small message every few seconds.
            if !failed && Instant::now() >= next_probe {
                next_probe = Instant::now() + RESYNC_PROBE;
                failed = c.write_line("STATE?").is_err();
            }
            if !failed {
                match c.drain() {
                    Ok(lines) => {
                        for l in &lines {
                            if Instant::now() < settle_until {
                                continue; // replies to our own commands, not drift
                            }
                            let Some(seen) = ring_seen(l, &target.looks) else { continue };
                            ring = Some(seen);
                            ring_ours = state_matches(l, &target) == Some(true);
                            let dark_as_wanted = target.is_off() && seen == RingSeen::Off;
                            if ring_ours || dark_as_wanted {
                                if yielding {
                                    yielding = false;
                                    set_status(true, label, &brownout_note(label, brownout_div));
                                }
                                continue;
                            }
                            if yields_to(&target, seen) {
                                if !yielding {
                                    yielding = true;
                                    dbg(&format!("ring shows another client's status, leaving it: {l}"));
                                    set_status(
                                        true,
                                        label,
                                        &format!("{} — showing another Mac's status", brownout_note(label, brownout_div)),
                                    );
                                }
                            } else {
                                dbg(&format!("ring drifted, re-asserting: {l}"));
                                yielding = false;
                                sent = None; // forces a full re-emit next turn
                            }
                        }
                    }
                    Err(_) => failed = true,
                }
            }
            if failed {
                dbg("emit/drain failed");
                // Dropping `conn` closes the socket / releases the serial port.
                conn = None;
                sent = None;
                ring = None;
                ring_ours = false;
                yielding = false;
                fails = fails.saturating_add(1);
                next_try = Instant::now() + backoff(fails);
                set_status(false, "", "Ring disconnected — will keep looking");
            }
        }
    }
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;
    fn ev(secs_ago: i64, event: &str, key: &str) -> (i64, String, String) {
        (NOW - secs_ago, event.to_string(), key.to_string())
    }
    fn pick(e: &[(i64, String, String)]) -> Option<&'static str> {
        pick_event(e, NOW, &Cfg::default())
    }

    fn default_looks() -> Vec<Look> {
        LADDER
            .iter()
            .enumerate()
            .map(|(i, r)| Look { rank: i, pattern: r.pattern.into(), color: r.color.into() })
            .collect()
    }

    fn target(rank: usize) -> LedState {
        LedState {
            transport: "auto".into(),
            host: "h".into(),
            pattern: "x".into(),
            color: "000000".into(),
            color2: "000000".into(),
            speed: 50,
            bright: 5,
            rank,
            looks: default_looks(),
        }
    }

    #[test]
    fn ring_state_is_recognised_on_our_ladder() {
        let looks = default_looks();
        let q = LADDER.iter().position(|r| r.event == "question").unwrap();
        let line = format!(r#"STATE {{"mode":"{}","bright":5,"speed":75,"color":"{}"}}"#, LADDER[q].pattern, LADDER[q].color);
        assert_eq!(ring_seen(&line, &looks), Some(RingSeen::Rung(q)));
        assert_eq!(ring_seen(r#"STATE {"mode":"off","color":"000000"}"#, &looks), Some(RingSeen::Off));
        assert_eq!(ring_seen(r#"STATE {"mode":"rainbow","color":"ff0000"}"#, &looks), Some(RingSeen::Foreign));
        // Same pattern, someone else's colour: not one of ours.
        let line = format!(r#"STATE {{"mode":"{}","color":"123456"}}"#, LADDER[q].pattern);
        assert_eq!(ring_seen(&line, &looks), Some(RingSeen::Foreign));
        assert_eq!(ring_seen("OK mode off", &looks), None);
    }

    #[test]
    fn shared_ring_yields_to_equal_or_more_urgent_status_only() {
        let q = LADDER.iter().position(|r| r.event == "question").unwrap();
        let w = LADDER.iter().position(|r| r.event == "working").unwrap();
        // Idle Mac: never fights a lit ring, does clear reboot defaults.
        assert!(yields_to(&target(OFF_RANK), RingSeen::Rung(w)));
        assert!(!yields_to(&target(OFF_RANK), RingSeen::Foreign));
        // Working Mac yields to another Mac's question, and to another
        // Mac's working (equal rank — no flicker), but not to a dark ring.
        assert!(yields_to(&target(w), RingSeen::Rung(q)));
        assert!(yields_to(&target(w), RingSeen::Rung(w)));
        assert!(!yields_to(&target(w), RingSeen::Off));
        assert!(!yields_to(&target(w), RingSeen::Foreign));
        // A question overrides another Mac's working.
        assert!(!yields_to(&target(q), RingSeen::Rung(w)));
    }

    #[test]
    fn no_events_means_ring_off() {
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn stale_sessions_are_dropped() {
        assert_eq!(pick(&[ev(STALE_SECS + 1, "waiting", "s1")]), None);
    }

    #[test]
    fn session_end_removes_a_session() {
        assert_eq!(pick(&[ev(20, "waiting", "s1"), ev(10, "session-end", "s1")]), None);
    }

    #[test]
    fn last_event_wins_per_session() {
        assert_eq!(pick(&[ev(30, "waiting", "s1"), ev(10, "stop", "s1")]), Some("stop"));
    }

    #[test]
    fn question_outranks_everything() {
        let e = [ev(5, "waiting", "s1"), ev(4, "question", "s2"), ev(3, "failure", "s3")];
        assert_eq!(pick(&e), Some("question"));
    }

    #[test]
    fn waiting_outranks_failure_and_working() {
        let e = [ev(5, "working", "s1"), ev(4, "failure", "s2"), ev(3, "waiting", "s3")];
        assert_eq!(pick_event(&e, NOW, &all_on()), Some("waiting"));
    }

    /// Ranking tests must not depend on which rungs happen to default on.
    fn all_on() -> Cfg {
        let mut cfg = Cfg::default();
        for look in cfg.events.values_mut() {
            look.enabled = true;
        }
        cfg
    }

    #[test]
    fn transient_events_flash_then_go_dark() {
        assert_eq!(pick(&[ev(5, "failure", "s1")]), Some("failure"));
        assert_eq!(pick_event(&[ev(5, "task-done", "s1")], NOW, &all_on()), Some("task-done"));
        assert_eq!(pick(&[ev(5, "stop", "s1")]), Some("stop"));
        // Past the hold window the flash is over: a failed session is still a
        // busy one (falls back to the working colour), a finished one is dark.
        assert_eq!(pick(&[ev(TRANSIENT_HOLD_SECS + 5, "failure", "s1")]), Some("working"));
        assert_eq!(pick(&[ev(TRANSIENT_HOLD_SECS + 5, "stop", "s1")]), None);
        // With working off the ring is dark once the flash has passed.
        let mut cfg = Cfg::default();
        cfg.events.get_mut("working").unwrap().enabled = false;
        assert_eq!(pick_event(&[ev(TRANSIENT_HOLD_SECS + 5, "failure", "s1")], NOW, &cfg), None);
    }

    #[test]
    fn mdns_packet_is_a_legacy_unicast_a_query() {
        let q = mdns_packet("ledring.local").unwrap();
        assert_eq!(&q[..12], &[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&q[12..], b"\x07ledring\x05local\x00\x00\x01\x00\x01");
        assert!(mdns_packet("ledring.local.").is_some());
        assert!(mdns_packet("example.com").is_none());
        assert!(mdns_packet("192.168.1.5").is_none());
    }

    #[test]
    fn mdns_answer_finds_the_a_record() {
        // Response: id 0, QR set, qd=1, an=1; question echoed; answer uses a
        // compression pointer back to the question name.
        let mut r = vec![0, 0, 0x84, 0, 0, 1, 0, 1, 0, 0, 0, 0];
        r.extend_from_slice(b"\x07ledring\x05local\x00\x00\x01\x00\x01");
        r.extend_from_slice(&[0xC0, 12, 0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4, 192, 168, 1, 5]);
        assert_eq!(mdns_answer(&r, "ledring.local"), Some(Ipv4Addr::new(192, 168, 1, 5)));
        assert_eq!(mdns_answer(&r, "other.local"), None);
        // A query (QR clear) or a truncated packet is never an answer.
        let mut q = r.clone();
        q[2] = 0;
        assert_eq!(mdns_answer(&q, "ledring.local"), None);
        assert_eq!(mdns_answer(&r[..r.len() - 2], "ledring.local"), None);
        // A pointer loop must not hang.
        let mut l = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        l.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 0, 0, 4, 1, 2, 3, 4]);
        assert_eq!(mdns_answer(&l, "x.local"), None);
    }

    /// Needs the ring on the LAN: `cargo test --lib -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn discovery_against_real_hardware() {
        let via_mdns = mdns_query("ledring.local");
        eprintln!("own mDNS query   -> {via_mdns:?}");
        let local = local_ipv4();
        eprintln!("local address    -> {local:?}");
        let via_scan = local.and_then(scan_subnet);
        eprintln!("subnet sweep     -> {via_scan:?}");
        // A name nobody answers must fall through the ladder to the sweep.
        let fallback = resolve_ring("nosuchring.local");
        eprintln!("bogus name       -> {fallback:?}");
        assert!(via_mdns.is_some(), "ring did not answer a legacy unicast mDNS query");
        assert_eq!(via_scan, via_mdns);
        assert_eq!(fallback, via_mdns);
        assert!(via_mdns.map(is_ring).unwrap_or(false));
        let rep = discover("ledring.local");
        for st in &rep.steps {
            eprintln!("  [{}] {:<28} {}", if st.ok { "ok" } else { "--" }, st.label, st.detail);
        }
        eprintln!("found={} hint={:?}", rep.found, rep.hint);
        assert_eq!(rep.found, via_mdns.unwrap().to_string());
        assert!(rep.hint.is_empty());
        let miss = discover("nosuchring.local");
        assert_eq!(miss.found, rep.found, "bogus name must still end at the ring via the sweep");
    }

    #[test]
    fn working_is_the_default_busy_colour_and_can_be_turned_off() {
        assert_eq!(pick(&[ev(5, "working", "s1")]), Some("working"));
        // Status, not notification: it stays lit as long as the agent is busy.
        assert_eq!(pick(&[ev(3600, "working", "s1")]), Some("working"));
        let mut cfg = Cfg::default();
        cfg.events.get_mut("working").unwrap().enabled = false;
        assert_eq!(pick_event(&[ev(5, "working", "s1")], NOW, &cfg), None);
    }

    #[test]
    fn needs_you_states_stay_lit_until_answered() {
        // An hour-old unanswered question is still a notification.
        assert_eq!(pick(&[ev(3600, "question", "s1")]), Some("question"));
        let on = all_on();
        assert_eq!(pick_event(&[ev(3600, "waiting", "s1")], NOW, &on), Some("waiting"));
        // Answering it (a new event for that session) clears it; with working
        // off the ring goes dark.
        let mut off = all_on();
        off.events.get_mut("working").unwrap().enabled = false;
        assert_eq!(pick_event(&[ev(3600, "waiting", "s1"), ev(3000, "working", "s1")], NOW, &off), None);
    }

    #[test]
    fn quiet_by_default_events_stay_quiet() {
        // compact and session-start default to LED off, like their banners;
        // waiting and task-done are off too (see the LADDER note). A disabled
        // rung is skipped, so the ring shows the session's standing instead:
        // compacting/task-done sessions are busy (working), a fresh one is dark.
        assert_eq!(pick(&[ev(5, "compact", "s1")]), Some("working"));
        assert_eq!(pick(&[ev(5, "task-done", "s1")]), Some("working"));
        assert_eq!(pick(&[ev(5, "session-start", "s1")]), None);
        assert_eq!(pick(&[ev(5, "waiting", "s1")]), None);
        let mut cfg = Cfg::default();
        cfg.events.get_mut("working").unwrap().enabled = false;
        for e in ["compact", "session-start", "waiting", "task-done"] {
            assert_eq!(pick_event(&[ev(5, e, "s1")], NOW, &cfg), None, "{e}");
        }
    }

    #[test]
    fn disabling_an_event_falls_through_to_the_next_rung() {
        let mut cfg = Cfg::default();
        cfg.events.get_mut("waiting").unwrap().enabled = false;
        cfg.events.get_mut("working").unwrap().enabled = true;
        let e = [ev(5, "working", "s1"), ev(3, "waiting", "s2")];
        assert_eq!(pick_event(&e, NOW, &cfg), Some("working"));
        // Everything off: the ring is dark even with live sessions.
        for l in cfg.events.values_mut() {
            l.enabled = false;
        }
        assert_eq!(pick_event(&e, NOW, &cfg), None);
    }

    #[test]
    fn colour_input_is_normalised_and_validated() {
        assert_eq!(clean_color("#FFB000"), Some("ffb000".into()));
        assert_eq!(clean_color("ffb000"), Some("ffb000".into()));
        assert_eq!(clean_color("#fff"), None);
        assert_eq!(clean_color("gggggg"), None);
    }

    #[test]
    fn quiet_hours_handles_overnight_ranges() {
        let cfg = Cfg {
            quiet_enabled: true,
            quiet_start: "22:00".into(),
            quiet_end: "08:00".into(),
            ..Cfg::default()
        };
        assert!(in_quiet_hours(&cfg, 23 * 60));
        assert!(in_quiet_hours(&cfg, 2 * 60));
        assert!(!in_quiet_hours(&cfg, 12 * 60));
        let day = Cfg { quiet_start: "09:00".into(), quiet_end: "17:00".into(), ..cfg };
        assert!(in_quiet_hours(&day, 12 * 60));
        assert!(!in_quiet_hours(&day, 20 * 60));
    }

    #[test]
    fn state_matches_compares_every_field() {
        let want = LedState {
            transport: "auto".into(),
            host: "h".into(),
            pattern: "breathe".into(),
            color: "ffb000".into(),
            color2: "000000".into(),
            speed: 45,
            bright: 60,
            rank: 1,
            looks: vec![],
        };
        let good = r#"STATE {"mode":"breathe","bright":60,"speed":45,"color":"ffb000","color2":"000000"}"#;
        assert_eq!(state_matches(good, &want), Some(true));
        let drifted = r#"STATE {"mode":"rainbow","bright":60,"speed":45,"color":"ffb000","color2":"000000"}"#;
        assert_eq!(state_matches(drifted, &want), Some(false));
        // Anything that isn't a STATE line must not be read as drift.
        assert_eq!(state_matches("OK mode breathe", &want), None);
    }
}
