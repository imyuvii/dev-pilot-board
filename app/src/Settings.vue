<script setup lang="ts">
import { onMounted, ref, watch, computed } from "vue";
import { invoke } from "@tauri-apps/api/core";
import {
  readTextFile,
  writeTextFile,
  exists,
  BaseDirectory,
} from "@tauri-apps/plugin-fs";

const CONFIG_FILE = ".claude/notify-config.json";

const SOUNDS = [
  "Basso", "Blow", "Bottle", "Frog", "Funk", "Glass", "Hero",
  "Morse", "Ping", "Pop", "Purr", "Sosumi", "Submarine", "Tink",
];

const EVENTS = [
  { key: "stop", label: "Done responding", emoji: "⚪", sound: "Glass", banner: true },
  { key: "waiting", label: "Waiting for you", emoji: "🟡", sound: "Ping", banner: true },
  { key: "question", label: "Question asked", emoji: "❓", sound: "Hero", banner: true },
  { key: "failure", label: "Tool call failed", emoji: "🔴", sound: "Basso", banner: true },
  { key: "task-done", label: "Background task done", emoji: "📦", sound: "Submarine", banner: true },
  { key: "compact", label: "Context compacting", emoji: "🌀", sound: "Purr", banner: false },
  { key: "session-start", label: "Session started", emoji: "🚀", sound: "Pop", banner: false },
];

interface EventCfg {
  sound: string;
  sound_enabled: boolean;
  banner_enabled: boolean;
}

interface Config {
  mute: { claude: boolean; copilot: boolean };
  quiet_hours: { enabled: boolean; start: string; end: string };
  events: Record<string, EventCfg>;
}

function defaults(): Config {
  const events: Record<string, EventCfg> = {};
  for (const e of EVENTS) {
    events[e.key] = { sound: e.sound, sound_enabled: true, banner_enabled: e.banner };
  }
  return {
    mute: { claude: false, copilot: false },
    quiet_hours: { enabled: false, start: "22:00", end: "08:00" },
    events,
  };
}

const config = ref<Config>(defaults());
const loaded = ref(false);
let saveTimer: number | undefined;

function fmt12(v: string): string {
  const [h, mm] = v.split(":").map(Number);
  const ap = h >= 12 ? "PM" : "AM";
  const h12 = h % 12 || 12;
  return `${h12}:${String(mm).padStart(2, "0")} ${ap}`;
}

const timeOpts = computed(() => {
  const opts: { v: string; label: string }[] = [];
  for (let h = 0; h < 24; h++)
    for (const mm of ["00", "30"])
      opts.push({ v: `${String(h).padStart(2, "0")}:${mm}`, label: fmt12(`${String(h).padStart(2, "0")}:${mm}`) });
  // keep a stored non-half-hour value selectable
  for (const v of [config.value.quiet_hours.start, config.value.quiet_hours.end])
    if (!opts.some((o) => o.v === v)) opts.push({ v, label: fmt12(v) });
  return opts.sort((a, b) => a.v.localeCompare(b.v));
});

const quietSummary = computed(
  () =>
    `Sounds muted ${fmt12(config.value.quiet_hours.start)} – ${fmt12(config.value.quiet_hours.end)}. Banners follow their own toggles.`
);

onMounted(async () => {
  try {
    if (await exists(CONFIG_FILE, { baseDir: BaseDirectory.Home })) {
      const text = await readTextFile(CONFIG_FILE, { baseDir: BaseDirectory.Home });
      const stored = JSON.parse(text);
      const merged = defaults();
      merged.mute = { ...merged.mute, ...(stored.mute ?? {}) };
      if (stored.master_mute === true) {
        // migrate legacy single switch
        merged.mute.claude = true;
        merged.mute.copilot = true;
      }
      merged.quiet_hours = { ...merged.quiet_hours, ...(stored.quiet_hours ?? {}) };
      for (const key of Object.keys(merged.events)) {
        merged.events[key] = { ...merged.events[key], ...(stored.events?.[key] ?? {}) };
      }
      config.value = merged;
    }
  } catch (e) {
    console.error("failed to load config", e);
  }
  loaded.value = true;
});

watch(
  config,
  () => {
    if (!loaded.value) return;
    clearTimeout(saveTimer);
    saveTimer = window.setTimeout(async () => {
      try {
        await writeTextFile(
          CONFIG_FILE,
          JSON.stringify(config.value, null, 2),
          { baseDir: BaseDirectory.Home }
        );
      } catch (e) {
        console.error("failed to save config", e);
      }
    }, 300);
  },
  { deep: true }
);

function preview(sound: string) {
  invoke("play_sound", { sound });
}
function setTone(key: string, ev: globalThis.Event) {
  const tone = (ev.target as HTMLSelectElement).value;
  config.value.events[key].sound = tone;
  preview(tone);
}
</script>

<template>
  <div class="settings">
    <section class="card list">
      <div class="row split">
        <span class="row-label">✳️&nbsp; Mute Claude notifications</span>
        <button
          class="switch" role="switch" :aria-checked="config.mute.claude"
          :class="{ on: config.mute.claude }"
          @click="config.mute.claude = !config.mute.claude"
        ><span class="knob" /></button>
      </div>
      <div class="row">
        <span class="row-label">🤖&nbsp; Mute Copilot notifications</span>
        <button
          class="switch" role="switch" :aria-checked="config.mute.copilot"
          :class="{ on: config.mute.copilot }"
          @click="config.mute.copilot = !config.mute.copilot"
        ><span class="knob" /></button>
      </div>
    </section>

    <section class="card list">
      <div class="row">
        <span class="row-label">🌙&nbsp; Quiet hours <span class="row-sub">no sounds, banners still shown</span></span>
        <button
          class="switch" role="switch" :aria-checked="config.quiet_hours.enabled"
          :class="{ on: config.quiet_hours.enabled }"
          @click="config.quiet_hours.enabled = !config.quiet_hours.enabled"
        ><span class="knob" /></button>
      </div>
      <template v-if="config.quiet_hours.enabled">
        <div class="times">
          <span class="mono-label">FROM</span>
          <select v-model="config.quiet_hours.start">
            <option v-for="t in timeOpts" :key="'f' + t.v" :value="t.v">{{ t.label }}</option>
          </select>
          <span class="mono-label">TO</span>
          <select v-model="config.quiet_hours.end">
            <option v-for="t in timeOpts" :key="'t' + t.v" :value="t.v">{{ t.label }}</option>
          </select>
        </div>
        <div class="quiet-summary">{{ quietSummary }}</div>
      </template>
    </section>

    <section class="card grid-card">
      <div class="ev-grid head">
        <span class="mono-label lg">EVENTS</span>
        <span class="mono-label ctr">SOUND</span>
        <span class="mono-label ctr">BANNER</span>
        <span class="mono-label">TONE</span>
        <span></span>
      </div>
      <div v-for="e in EVENTS" :key="e.key" class="ev-grid row-line" :class="{ off: config.mute.claude && config.mute.copilot }">
        <span class="ev-name">{{ e.emoji }}&nbsp; {{ e.label }}</span>
        <button
          class="check" title="Sound" :class="{ on: config.events[e.key].sound_enabled }"
          @click="config.events[e.key].sound_enabled = !config.events[e.key].sound_enabled"
        >{{ config.events[e.key].sound_enabled ? "✓" : "" }}</button>
        <button
          class="check" title="Banner" :class="{ on: config.events[e.key].banner_enabled }"
          @click="config.events[e.key].banner_enabled = !config.events[e.key].banner_enabled"
        >{{ config.events[e.key].banner_enabled ? "✓" : "" }}</button>
        <select :value="config.events[e.key].sound" @change="setTone(e.key, $event)">
          <option v-for="s in SOUNDS" :key="s" :value="s">{{ s }}</option>
        </select>
        <button class="play" title="Preview tone" @click="preview(config.events[e.key].sound)">▶</button>
      </div>
    </section>

    <p class="hint">
      Changes save automatically and apply to the next notification — no restart needed.
    </p>
  </div>
</template>

<style scoped>
.settings { display: flex; flex-direction: column; gap: 10px; }

.card {
  background: var(--card); border: 1px solid var(--line);
  border-radius: 12px;
}
.card.list { padding: 4px 13px; }
.card.grid-card { padding: 11px 13px 8px; }

.row {
  display: flex; align-items: center; justify-content: space-between;
  padding: 9px 0;
}
.row.split { border-bottom: 1px solid var(--line); }
.row-label { font-size: 13.5px; font-weight: 500; }
.row-sub { color: var(--ink3); font-weight: 400; font-size: 12px; }

.switch {
  width: 38px; height: 22px; border-radius: 11px; border: none; cursor: pointer;
  background: var(--toggleOff); position: relative; transition: background 0.15s;
  flex-shrink: 0;
}
.switch.on { background: var(--accent); }
.switch .knob {
  position: absolute; top: 2px; left: 2px; width: 18px; height: 18px;
  border-radius: 50%; background: #fff;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.35); transition: left 0.15s;
}
.switch.on .knob { left: 18px; }

.times {
  display: flex; align-items: center; gap: 10px; padding: 2px 0 8px;
}
.times select {
  flex: 1; background: var(--card2); color: var(--ink);
  border: 1px solid var(--line); border-radius: 8px; padding: 6px 8px;
  font: 500 12.5px "Geist Mono", monospace; cursor: pointer;
}
.quiet-summary { font-size: 11.5px; color: var(--ink3); padding: 0 0 11px; }

.mono-label {
  font: 600 9.5px "Geist Mono", monospace; letter-spacing: 0.08em; color: var(--ink3);
}
.mono-label.lg { font-size: 10.5px; letter-spacing: 0.12em; }
.mono-label.ctr { text-align: center; }

.ev-grid {
  display: grid; grid-template-columns: 1fr 44px 48px 104px 30px;
  align-items: center; gap: 6px;
}
.ev-grid.head { padding-bottom: 7px; }
.ev-grid.row-line { padding: 6px 0; border-top: 1px solid var(--line); }
.ev-grid.off { opacity: 0.45; }

.ev-name {
  font-size: 12.5px; font-weight: 500;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}

.check {
  width: 20px; height: 20px; justify-self: center; border-radius: 6px;
  border: 1px solid var(--line); cursor: pointer;
  background: transparent; color: var(--accentInk);
  font: 700 11px "Outfit", system-ui, sans-serif;
  display: flex; align-items: center; justify-content: center;
}
.check.on { background: var(--accent); }

.ev-grid select {
  background: var(--card2); color: var(--ink);
  border: 1px solid var(--line); border-radius: 7px; padding: 4px 5px;
  font: 500 11.5px "Geist Mono", monospace; cursor: pointer; width: 100%;
}

.play {
  width: 24px; height: 24px; justify-self: center; border-radius: 7px;
  border: 1px solid var(--line); background: var(--card2); color: var(--ink2);
  cursor: pointer; font-size: 9px;
  display: flex; align-items: center; justify-content: center;
}
.play:hover { color: var(--accent); border-color: var(--accent); }

.hint {
  font-size: 11.5px; color: var(--ink3); text-align: center;
  padding: 2px 8px 4px; line-height: 1.5;
}
</style>
