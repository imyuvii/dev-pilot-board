// LED ring: config shape for the settings panel only.
//
// The ring's decision (which event wins, colour, pattern) deliberately does NOT
// live here. Dev Pilot Board is a menu-bar app whose window is hidden nearly all
// the time, and WebKit suspends timers in a hidden webview, so anything driven
// from the frontend stops a tick after launch. `src-tauri/src/led.rs` reads the
// event log and this config itself and owns the whole pipeline; the UI only
// edits settings and reads back a connection status.

export type LedTransport = "auto" | "wifi" | "ble" | "usb";

/** Per-event look — same shape of control as sound/banner in the events grid. */
export interface LedEventCfg {
  enabled: boolean;
  /** rrggbb, no leading '#'. */
  color: string;
  /** One of PATTERNS (the firmware's effect names). */
  pattern: string;
}

export interface LedCfg {
  enabled: boolean;
  transport: LedTransport;
  host: string;
  /** 0-160; the firmware's 5V/700mA power budget is why it stops at 160. */
  brightness: number;
  dim_in_quiet_hours: boolean;
  events: Record<string, LedEventCfg>;
}

export interface LedStatus {
  connected: boolean;
  transport: string;
  detail: string;
}

/** One rung of the discovery ladder, as returned by the `led_discover` command. */
export interface DiscoveryStep {
  label: string;
  ok: boolean;
  detail: string;
}

export interface DiscoveryReport {
  version: string;
  host: string;
  local_ip: string;
  steps: DiscoveryStep[];
  /** Address the ring answered on; empty when nothing was found. */
  found: string;
  /** What to try next; empty when found. */
  hint: string;
}

/**
 * Ranked most urgent first — the ring shows one thing, and this is the order.
 * KEEP IN SYNC with `LADDER` in led.rs (defaults, order, which are one-off).
 */
export const LED_EVENTS: {
  key: string;
  label: string;
  emoji: string;
  hint?: string;
  enabled: boolean;
  color: string;
  pattern: string;
}[] = [
  { key: "question", label: "Question asked", emoji: "❓", enabled: true, color: "ff00aa", pattern: "chase" },
  { key: "waiting", label: "Waiting for you", emoji: "🟡", enabled: false, color: "0061ff", pattern: "chase" },
  { key: "failure", label: "Tool call failed", emoji: "🔴", hint: "30s", enabled: true, color: "ff2020", pattern: "fire" },
  { key: "task-done", label: "Background task done", emoji: "📦", hint: "30s", enabled: false, color: "00d8ff", pattern: "juggle" },
  { key: "compact", label: "Context compacting", emoji: "🌀", hint: "30s", enabled: false, color: "9b59b6", pattern: "breathe" },
  { key: "session-start", label: "Session started", emoji: "🚀", hint: "30s", enabled: false, color: "ffffff", pattern: "chase" },
  { key: "working", label: "Working", emoji: "🟢", hint: "status, stays on", enabled: true, color: "1e90ff", pattern: "chase" },
  { key: "stop", label: "Done responding", emoji: "⚪", hint: "30s", enabled: true, color: "4f7a28", pattern: "breathe" },
];

/** The firmware's effect list (ring_control.ino), minus `manual`. */
export const PATTERNS = [
  "solid", "breathe", "comet", "chase", "theater", "sparkle", "wipe",
  "rainbow", "rainbowCycle", "gradient", "fire", "confetti", "juggle", "bpm",
];

/** Must match `Cfg::default()` in led.rs. */
export function ledDefaults(): LedCfg {
  const events: Record<string, LedEventCfg> = {};
  for (const e of LED_EVENTS) {
    events[e.key] = { enabled: e.enabled, color: e.color, pattern: e.pattern };
  }
  return {
    enabled: false, // opt-in: off means no mDNS probe and no serial port opened
    transport: "auto",
    host: "ledring.local",
    brightness: 5,
    dim_in_quiet_hours: true,
    events,
  };
}
