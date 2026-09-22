<script setup lang="ts">
import { onMounted, onUnmounted, ref, computed } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { readTextFile, exists, BaseDirectory } from "@tauri-apps/plugin-fs";
import Settings from "./Settings.vue";

const view = ref<"sessions" | "settings">("sessions");

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
  "session-start": "idle",
  working: "working",
  compact: "working",
  failure: "working",
  "task-done": "working",
  waiting: "waiting",
  question: "question",
  stop: "done",
};

const STATUS_META: Record<string, { icon: string; label: string; rank: number }> = {
  question: { icon: "❓", label: "Has a question", rank: 0 },
  waiting: { icon: "🟡", label: "Waiting for you", rank: 1 },
  working: { icon: "🟢", label: "Working", rank: 2 },
  done: { icon: "⚪", label: "Done", rank: 3 },
  idle: { icon: "⚪", label: "Idle", rank: 4 },
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
  working: "🟢",
};

const sessions = computed<Session[]>(() => {
  const map = new Map<string, Session>();
  const ended = new Set<string>();
  for (const e of events.value) {
    const key = e.session || e.cwd || e.project;
    if (e.event === "session-end") {
      ended.add(key);
      map.delete(key);
      continue;
    }
    const status = STATUS_MAP[e.event];
    if (!status) continue;
    ended.delete(key);
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
      <h1>Dev Pilot Board</h1>
      <nav class="tabs">
        <button :class="{ active: view === 'sessions' }" @click="view = 'sessions'">Sessions</button>
        <button :class="{ active: view === 'settings' }" @click="view = 'settings'">Settings</button>
      </nav>
      <span class="count" v-if="view === 'sessions'">{{ sessions.length }} session{{ sessions.length === 1 ? "" : "s" }}</span>
    </header>

    <Settings v-if="view === 'settings'" />

    <section class="sessions" v-if="view === 'sessions'">
      <div v-if="sessions.length === 0" class="empty">
        No active sessions.<br />
        <small>Start a Claude Code session and it will appear here.</small>
      </div>
      <div v-for="s in sessions" :key="s.key" class="session" :class="s.status">
        <span class="icon">{{ STATUS_META[s.status].icon }}</span>
        <div class="info">
          <div class="project">
            {{ s.project }}
            <span class="badge" :class="s.source">{{ s.source === "copilot" ? "Copilot" : "Claude" }}</span>
          </div>
          <div class="detail">
            {{ s.msg || STATUS_META[s.status].label }}
          </div>
        </div>
        <span class="time">{{ ago(s.lastTs) }}</span>
      </div>
    </section>

    <section class="history" v-if="view === 'sessions' && history.length">
      <h2>Recent events</h2>
      <div v-for="(e, i) in history" :key="i" class="event">
        <span class="icon">{{ EVENT_ICONS[e.event] || "•" }}</span>
        <span class="name">
          {{ e.project }}<span v-if="e.source === 'copilot'" class="badge copilot">Copilot</span>
        </span>
        <span class="what">{{ e.msg || e.event }}</span>
        <span class="time">{{ ago(Date.parse(e.ts)) }}</span>
      </div>
    </section>
  </main>
</template>

<style>
:root {
  color-scheme: dark;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
* { margin: 0; padding: 0; box-sizing: border-box; }
body { background: #16161d; color: #e8e8ec; }

.wrap { padding: 14px; display: flex; flex-direction: column; gap: 14px; height: 100vh; overflow-y: auto; }

header { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
h1 { font-size: 15px; font-weight: 600; }
.tabs { display: flex; gap: 2px; background: #1f1f28; border-radius: 8px; padding: 2px; }
.tabs button {
  background: transparent; color: #8a8a93; border: none; border-radius: 6px;
  padding: 4px 10px; font-size: 11px; cursor: pointer;
}
.tabs button.active { background: #2f2f3d; color: #e8e8ec; }
.count { font-size: 11px; color: #8a8a93; }

.sessions { display: flex; flex-direction: column; gap: 6px; }
.empty { text-align: center; color: #8a8a93; font-size: 13px; padding: 24px 0; line-height: 1.8; }

.session {
  display: flex; align-items: center; gap: 10px;
  background: #1f1f28; border-radius: 10px; padding: 10px 12px;
  border: 1px solid transparent;
}
.session.question { border-color: #7c5cff; }
.session.waiting { border-color: #b8860b; }
.session .icon { font-size: 16px; }
.session .info { flex: 1; min-width: 0; }
.session .project { font-size: 13px; font-weight: 600; }
.badge {
  font-size: 9px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.04em;
  border-radius: 4px; padding: 1px 5px; margin-left: 6px; vertical-align: 1px;
}
.badge.claude { background: #3a2f5a; color: #b9a5ff; }
.badge.copilot { background: #1d3a5f; color: #7fb8ff; }
.session .detail { font-size: 11px; color: #8a8a93; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.session .time { font-size: 11px; color: #8a8a93; flex-shrink: 0; }

.history h2 { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: #8a8a93; margin-bottom: 8px; }
.event {
  display: flex; align-items: center; gap: 8px;
  font-size: 12px; padding: 4px 2px; color: #c9c9d1;
}
.event .icon { width: 18px; text-align: center; }
.event .name { font-weight: 600; flex-shrink: 0; }
.event .what { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: #8a8a93; }
.event .time { font-size: 10px; color: #6a6a72; flex-shrink: 0; }
</style>
