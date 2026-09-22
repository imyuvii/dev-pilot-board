<script setup lang="ts">
import { onMounted, onUnmounted, ref, computed } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { readTextFile, exists, BaseDirectory } from "@tauri-apps/plugin-fs";
import Settings from "./Settings.vue";

const view = ref<"sessions" | "settings">("sessions");

// first-launch hook setup
const hooksOk = ref(true);
const settingUp = ref(false);
const setupMsg = ref("");

async function checkHooks() {
  try {
    hooksOk.value = await invoke<boolean>("hooks_status");
  } catch {
    hooksOk.value = true; // fail quiet — never block the dashboard
  }
}
async function runSetup() {
  settingUp.value = true;
  setupMsg.value = "";
  try {
    setupMsg.value = await invoke<string>("setup_hooks");
    hooksOk.value = true;
  } catch (e) {
    setupMsg.value = String(e);
  } finally {
    settingUp.value = false;
  }
}

const STATE_FILE = ".claude/notify-state.jsonl";
const STALE_MS = 12 * 60 * 60 * 1000; // hide sessions idle > 12h

interface Event {
  ts: string;
  event: string;
  project: string;
  cwd: string;
  session: string;
  msg: string;
  source?: string;
}

interface Session {
  key: string;
  project: string;
  status: string;
  lastTs: number;
  msg: string;
  source: string;
}

const events = ref<Event[]>([]);
const now = ref(Date.now());
let pollTimer: number | undefined;
let clockTimer: number | undefined;

// event -> session status
const STATUS_MAP: Record<string, string> = {
  "session-start": "done",
  working: "working",
  compact: "working",
  failure: "working",
  "task-done": "working",
  waiting: "waiting",
  question: "question",
  stop: "done",
};

const STATUS_META: Record<string, { emoji: string; label: string; rank: number }> = {
  question: { emoji: "❓", label: "Needs an answer", rank: 0 },
  waiting: { emoji: "🟡", label: "Waiting for input", rank: 1 },
  working: { emoji: "🟢", label: "Working", rank: 2 },
  done: { emoji: "⚪", label: "Idle", rank: 3 },
};

const EVENT_ICONS: Record<string, string> = {
  stop: "⚪",
  waiting: "🟡",
  question: "❓",
  failure: "🔴",
  "task-done": "📦",
  compact: "🌀",
  "session-start": "🚀",
  "session-end": "🏁",
};

const EVENT_LABELS: Record<string, string> = {
  stop: "done responding",
  waiting: "waiting for you",
  question: "asked a question",
  failure: "tool call failed",
  "task-done": "background task finished",
  compact: "compacting context",
  "session-start": "session started",
  "session-end": "session ended",
};

const sessions = computed<Session[]>(() => {
  const map = new Map<string, Session>();
  for (const e of events.value) {
    const key = e.session || e.cwd || e.project;
    if (e.event === "session-end") {
      map.delete(key);
      continue;
    }
    const status = STATUS_MAP[e.event];
    if (!status) continue;
    map.set(key, {
      key,
      project: e.project,
      status,
      lastTs: Date.parse(e.ts),
      msg: e.event === "waiting" ? e.msg : "",
      source: e.source || "claude",
    });
  }
  return [...map.values()]
    .filter((s) => now.value - s.lastTs < STALE_MS)
    .sort(
      (a, b) =>
        STATUS_META[a.status].rank - STATUS_META[b.status].rank ||
        b.lastTs - a.lastTs
    );
});

const history = computed(() =>
  [...events.value].reverse().filter((e) => e.event !== "working").slice(0, 25)
);

function ago(ts: number): string {
  const s = Math.max(0, Math.floor((now.value - ts) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  return `${Math.floor(s / 3600)}h ago`;
}

async function poll() {
  try {
    if (!(await exists(STATE_FILE, { baseDir: BaseDirectory.Home }))) return;
    const text = await readTextFile(STATE_FILE, { baseDir: BaseDirectory.Home });
    const parsed: Event[] = [];
    for (const line of text.split("\n")) {
      if (!line.trim()) continue;
      try {
        parsed.push(JSON.parse(line));
      } catch {
        /* skip malformed line */
      }
    }
    events.value = parsed;

    // Tray glyph: most urgent state across sessions
    const list = sessions.value;
    let glyph = "";
    if (list.some((s) => s.status === "question")) glyph = "?";
    else if (list.some((s) => s.status === "waiting")) glyph = "!";
    else if (list.some((s) => s.status === "working")) glyph = "…";
    await invoke("set_tray_title", { title: glyph });
  } catch (e) {
    console.error("poll failed", e);
  }
}

onMounted(() => {
  checkHooks();
  poll();
  pollTimer = window.setInterval(poll, 2000);
  clockTimer = window.setInterval(() => (now.value = Date.now()), 1000);
});
onUnmounted(() => {
  clearInterval(pollTimer);
  clearInterval(clockTimer);
});
</script>

<template>
  <main class="wrap">
    <header>
      <div class="brand">
        <span class="logo">🛩️</span>
        <span class="title">Dev Pilot Board</span>
      </div>
      <nav class="tabs">
        <button :class="{ active: view === 'sessions' }" @click="view = 'sessions'">Sessions</button>
        <button :class="{ active: view === 'settings' }" @click="view = 'settings'">Settings</button>
      </nav>
    </header>

    <div class="scroll">
      <Settings v-if="view === 'settings'" />

      <template v-if="view === 'sessions'">
        <div v-if="!hooksOk || setupMsg" class="setup-card">
          <template v-if="!hooksOk">
            <div class="setup-title">🔌 Connect your agents</div>
            <div class="setup-sub">
              One click installs the notification hooks for Claude Code and
              Copilot CLI (your settings are backed up first).
            </div>
            <button class="setup-btn" :disabled="settingUp" @click="runSetup">
              {{ settingUp ? "Setting up…" : "Set up hooks" }}
            </button>
            <div v-if="setupMsg" class="setup-msg">{{ setupMsg }}</div>
          </template>
          <template v-else>
            <div class="setup-title">✅ Hooks installed</div>
            <div class="setup-msg">{{ setupMsg }}</div>
          </template>
        </div>

        <div class="sec-head">
          <span class="sec-label">ACTIVE SESSIONS</span>
          <span class="sec-count">{{ sessions.length }}</span>
        </div>

        <div v-if="sessions.length === 0" class="empty">
          <div class="empty-chip">🛩️</div>
          <div class="empty-title">All quiet</div>
          <div class="empty-sub">
            No active agent sessions. Start Claude Code or<br />Copilot CLI and
            sessions appear here automatically.
          </div>
        </div>

        <div class="cards" v-else>
          <div v-for="s in sessions" :key="s.key" class="session" :class="s.status">
            <span class="s-emoji" :title="STATUS_META[s.status].label">{{ STATUS_META[s.status].emoji }}</span>
            <div class="s-info">
              <div class="s-top">
                <span class="s-project">{{ s.project }}</span>
                <span class="badge" :class="s.source">{{ s.source === "copilot" ? "COPILOT" : "CLAUDE" }}</span>
              </div>
              <div class="s-msg">{{ s.msg || STATUS_META[s.status].label }}</div>
            </div>
            <span class="s-ago">{{ ago(s.lastTs) }}</span>
          </div>
        </div>

        <div class="sec-head feed-head" v-if="history.length">
          <span class="sec-label">RECENT EVENTS</span>
        </div>
        <div class="feed" v-if="history.length">
          <div v-for="(e, i) in history" :key="i" class="event">
            <span class="e-emoji">{{ EVENT_ICONS[e.event] || "•" }}</span>
            <span class="e-project">{{ e.project }}</span>
            <span v-if="e.source === 'copilot'" class="badge sm copilot">COPILOT</span>
            <span class="e-label">{{ e.msg || EVENT_LABELS[e.event] || e.event }}</span>
            <span class="e-ago">{{ ago(Date.parse(e.ts)) }}</span>
          </div>
        </div>
      </template>
    </div>
  </main>
</template>

<style>
:root {
  --win: #17191f;
  --card: rgba(255, 255, 255, 0.045);
  --card2: rgba(255, 255, 255, 0.08);
  --ink: #f2f4f7;
  --ink2: #a8afbb;
  --ink3: #6d7480;
  --line: rgba(255, 255, 255, 0.08);
  --accent: #34d399;
  --accentInk: #04251a;
  --warnBorder: rgba(250, 204, 21, 0.45);
  --qBorder: rgba(251, 113, 133, 0.5);
  --toggleOff: rgba(255, 255, 255, 0.12);
  --sel: rgba(255, 255, 255, 0.07);
  color-scheme: dark;
}
@media (prefers-color-scheme: light) {
  :root {
    --win: #fcfdfe;
    --card: rgba(9, 15, 25, 0.04);
    --card2: rgba(9, 15, 25, 0.075);
    --ink: #171a20;
    --ink2: #565e6b;
    --ink3: #8b93a1;
    --line: rgba(9, 15, 25, 0.09);
    --accent: #0d9d6d;
    --accentInk: #ffffff;
    --warnBorder: rgba(202, 138, 4, 0.55);
    --qBorder: rgba(225, 29, 72, 0.45);
    --toggleOff: rgba(9, 15, 25, 0.14);
    --sel: rgba(9, 15, 25, 0.06);
    color-scheme: light;
  }
}

* { margin: 0; padding: 0; box-sizing: border-box; }
html, body, #app { height: 100%; }
body {
  background: var(--win);
  color: var(--ink);
  font-family: "Outfit", system-ui, sans-serif;
  -webkit-font-smoothing: antialiased;
  overflow: hidden;
}
::-webkit-scrollbar { width: 8px; }
::-webkit-scrollbar-thumb { background: rgba(128, 134, 146, 0.35); border-radius: 4px; }
::-webkit-scrollbar-track { background: transparent; }

.wrap { height: 100%; display: flex; flex-direction: column; }

header {
  display: flex; align-items: center; justify-content: space-between;
  padding: 16px 18px 12px; flex-shrink: 0;
}
.brand { display: flex; align-items: center; gap: 9px; }
.logo {
  width: 26px; height: 26px; border-radius: 8px;
  background: linear-gradient(135deg, var(--accent), #1d9d74);
  display: flex; align-items: center; justify-content: center; font-size: 14px;
}
.title { font-weight: 700; font-size: 16px; letter-spacing: -0.01em; }

.tabs {
  display: flex; background: var(--card); border: 1px solid var(--line);
  border-radius: 9px; padding: 3px; gap: 2px;
}
.tabs button {
  border: none; border-radius: 6px; padding: 5px 12px;
  font: 600 12.5px "Outfit", system-ui, sans-serif; cursor: pointer;
  background: transparent; color: var(--ink2);
}
.tabs button.active { background: var(--accent); color: var(--accentInk); }

.scroll { flex: 1; overflow-y: auto; padding: 2px 14px 14px; }

.setup-card {
  background: var(--card); border: 1px solid var(--accent);
  border-radius: 12px; padding: 13px; margin-bottom: 12px;
}
.setup-title { font-weight: 600; font-size: 13.5px; }
.setup-sub { font-size: 12px; color: var(--ink2); margin-top: 3px; line-height: 1.5; }
.setup-btn {
  margin-top: 10px; border: none; border-radius: 8px; padding: 7px 14px;
  background: var(--accent); color: var(--accentInk);
  font: 600 12.5px "Outfit", system-ui, sans-serif; cursor: pointer;
}
.setup-btn:disabled { opacity: 0.6; cursor: default; }
.setup-msg { font-size: 11.5px; color: var(--ink2); margin-top: 8px; line-height: 1.5; }

.sec-head {
  display: flex; align-items: center; justify-content: space-between;
  padding: 2px 4px 8px;
}
.feed-head { padding-top: 16px; }
.sec-label {
  font: 600 11px "Geist Mono", monospace; letter-spacing: 0.12em; color: var(--ink3);
}
.sec-count { font: 500 11px "Geist Mono", monospace; color: var(--ink3); }

.cards { display: flex; flex-direction: column; gap: 8px; }
.session {
  display: flex; align-items: center; gap: 11px;
  background: var(--card); border: 1px solid var(--line);
  border-radius: 12px; padding: 11px 13px;
}
.session.waiting { border-color: var(--warnBorder); }
.session.question { border-color: var(--qBorder); }
.s-emoji { font-size: 15px; flex-shrink: 0; }
.s-info { flex: 1; min-width: 0; }
.s-top { display: flex; align-items: center; gap: 8px; }
.s-project {
  font-weight: 600; font-size: 13.5px;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.s-msg {
  font-size: 12px; color: var(--ink2); margin-top: 2px;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.s-ago { font: 500 11px "Geist Mono", monospace; color: var(--ink3); flex-shrink: 0; }

.badge {
  font: 600 9.5px "Geist Mono", monospace; letter-spacing: 0.08em;
  border-radius: 4px; padding: 2px 6px; flex-shrink: 0;
}
.badge.sm { font-size: 9px; padding: 1.5px 5px; }
.badge.claude { background: rgba(124, 108, 246, 0.25); color: #c4b9ff; }
.badge.copilot { background: rgba(79, 156, 249, 0.22); color: #a9cdff; }
@media (prefers-color-scheme: light) {
  .badge.claude { background: rgba(124, 108, 246, 0.16); color: #5b4bd6; }
  .badge.copilot { background: rgba(79, 156, 249, 0.16); color: #1e6fd9; }
}

.empty { text-align: center; padding: 36px 20px 32px; }
.empty-chip {
  width: 56px; height: 56px; margin: 0 auto 14px; border-radius: 16px;
  background: var(--card); border: 1px solid var(--line);
  display: flex; align-items: center; justify-content: center; font-size: 26px;
}
.empty-title { font-weight: 600; font-size: 15px; }
.empty-sub { font-size: 12.5px; color: var(--ink2); margin-top: 4px; line-height: 1.5; }

.feed { display: flex; flex-direction: column; }
.event {
  display: flex; align-items: center; gap: 9px;
  padding: 6.5px 4px; border-bottom: 1px solid var(--line);
}
.e-emoji { font-size: 11px; flex-shrink: 0; }
.e-project { font-weight: 600; font-size: 12px; flex-shrink: 0; }
.e-label {
  font-size: 12px; color: var(--ink2); flex: 1; min-width: 0;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.e-ago { font: 500 10.5px "Geist Mono", monospace; color: var(--ink3); flex-shrink: 0; }
</style>
