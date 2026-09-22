<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
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
  { key: "stop", label: "Done responding", icon: "⚪", sound: "Glass", banner: true },
  { key: "waiting", label: "Waiting for you", icon: "🟡", sound: "Ping", banner: true },
  { key: "question", label: "Question asked", icon: "❓", sound: "Hero", banner: true },
  { key: "failure", label: "Tool call failed", icon: "🔴", sound: "Basso", banner: true },
  { key: "task-done", label: "Background task done", icon: "📦", sound: "Submarine", banner: true },
  { key: "compact", label: "Context compacting", icon: "🌀", sound: "Purr", banner: false },
  { key: "session-start", label: "Session started", icon: "🚀", sound: "Pop", banner: false },
];

interface EventCfg {
  sound: string;
  sound_enabled: boolean;
  banner_enabled: boolean;
}

interface Config {
  master_mute: boolean;
  quiet_hours: { enabled: boolean; start: string; end: string };
  events: Record<string, EventCfg>;
}

function defaults(): Config {
  const events: Record<string, EventCfg> = {};
  for (const e of EVENTS) {
    events[e.key] = { sound: e.sound, sound_enabled: true, banner_enabled: e.banner };
  }
  return {
    master_mute: false,
    quiet_hours: { enabled: false, start: "22:00", end: "08:00" },
    events,
  };
}

const config = ref<Config>(defaults());
const loaded = ref(false);
let saveTimer: number | undefined;

onMounted(async () => {
  try {
    if (await exists(CONFIG_FILE, { baseDir: BaseDirectory.Home })) {
      const text = await readTextFile(CONFIG_FILE, { baseDir: BaseDirectory.Home });
      const stored = JSON.parse(text);
      const merged = defaults();
      merged.master_mute = stored.master_mute ?? merged.master_mute;
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
</script>

<template>
  <div class="settings">
    <section class="card">
      <label class="row master">
        <span>🔕 Mute all notifications</span>
        <input type="checkbox" v-model="config.master_mute" />
      </label>
    </section>

    <section class="card">
      <label class="row">
        <span>🌙 Quiet hours <small>(no sounds, banners still shown)</small></span>
        <input type="checkbox" v-model="config.quiet_hours.enabled" />
      </label>
      <div class="row times" v-if="config.quiet_hours.enabled">
        <label>From <input type="time" v-model="config.quiet_hours.start" /></label>
        <label>To <input type="time" v-model="config.quiet_hours.end" /></label>
      </div>
    </section>

    <section class="card">
      <h2>Events</h2>
      <div class="ev-head">
        <span class="ev-name"></span>
        <span class="col">Sound</span>
        <span class="col">Banner</span>
        <span class="ev-sound">Tone</span>
      </div>
      <div v-for="e in EVENTS" :key="e.key" class="ev-row" :class="{ off: config.master_mute }">
        <span class="ev-name">{{ e.icon }} {{ e.label }}</span>
        <span class="col">
          <input type="checkbox" v-model="config.events[e.key].sound_enabled" />
        </span>
        <span class="col">
          <input type="checkbox" v-model="config.events[e.key].banner_enabled" />
        </span>
        <span class="ev-sound">
          <select v-model="config.events[e.key].sound">
            <option v-for="s in SOUNDS" :key="s" :value="s">{{ s }}</option>
          </select>
          <button class="play" @click="preview(config.events[e.key].sound)" title="Preview">▶</button>
        </span>
      </div>
    </section>

    <p class="hint">
      Changes save automatically and apply to the next notification — no restart needed.
    </p>
  </div>
</template>

<style scoped>
.settings { display: flex; flex-direction: column; gap: 12px; }

.card { background: #1f1f28; border-radius: 10px; padding: 12px; }
.card h2 { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: #8a8a93; margin-bottom: 10px; }

.row { display: flex; align-items: center; justify-content: space-between; font-size: 13px; }
.row small { color: #8a8a93; font-size: 11px; }
.row.master { font-weight: 600; }
.row.times { justify-content: flex-start; gap: 16px; margin-top: 10px; font-size: 12px; color: #c9c9d1; }
.row.times input { margin-left: 6px; background: #16161d; color: #e8e8ec; border: 1px solid #33333f; border-radius: 6px; padding: 3px 6px; }

.ev-head, .ev-row {
  display: grid;
  grid-template-columns: 1fr 48px 48px 130px;
  align-items: center;
  gap: 4px;
}
.ev-head { font-size: 10px; color: #6a6a72; text-transform: uppercase; letter-spacing: 0.05em; margin-bottom: 6px; }
.ev-head .col, .ev-row .col { text-align: center; }
.ev-row { padding: 5px 0; font-size: 12px; }
.ev-row.off { opacity: 0.45; }
.ev-name { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.ev-sound { display: flex; align-items: center; gap: 4px; }
.ev-sound select {
  flex: 1; min-width: 0;
  background: #16161d; color: #e8e8ec;
  border: 1px solid #33333f; border-radius: 6px; padding: 3px 4px; font-size: 11px;
}
.play {
  background: #2a2a36; color: #e8e8ec; border: none; border-radius: 6px;
  width: 24px; height: 24px; cursor: pointer; font-size: 10px; flex-shrink: 0;
}
.play:hover { background: #3a3a4a; }

input[type="checkbox"] { accent-color: #7c5cff; width: 15px; height: 15px; }

.hint { font-size: 11px; color: #6a6a72; text-align: center; }
</style>
