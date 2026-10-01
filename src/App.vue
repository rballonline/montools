<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface InputOption { code: number; name: string }
interface KvmSlot { input: number; input_name: string; port: number; port_name: string }
interface KvmState { raw: number; ports: { port: number; name: string }[]; slots: KvmSlot[] }
interface MonitorStatus { key: string; model: string; inputs: InputOption[]; current_input: number | null; kvm: KvmState | null }
interface Config {
  monitor: string | null;
  thisInput: number | null;
  otherInput: number | null;
  thisName: string | null;
  otherName: string | null;
  hotkey: string | null;
  showAllMonitors: boolean;
}

const monitors = ref<MonitorStatus[]>([]);
const config = ref<Config>({
  monitor: null, thisInput: null, otherInput: null, thisName: null, otherName: null, hotkey: null, showAllMonitors: false,
});
const autostart = ref(false);
const loading = ref(true);
const busy = ref(false);
const error = ref("");
const notice = ref("");
const capturing = ref(false);

const monitor = computed(() => monitors.value.find((m) => m.key === config.value.monitor) ?? null);
const inputName = (code: number | null) => monitor.value?.inputs.find((i) => i.code === code)?.name ?? "—";
const hasKvm = computed(() => monitors.value.some((m) => m.kvm));
// Only KVM-capable monitors by default; fall back to all if none have a known KVM profile.
const visibleMonitors = computed(() =>
  config.value.showAllMonitors || !hasKvm.value
    ? monitors.value
    : monitors.value.filter((m) => m.kvm || m.key === config.value.monitor),
);
const hiddenCount = computed(() => monitors.value.length - visibleMonitors.value.length);
/** Friendly name of the computer on an input, or null if it isn't one of the two configured. */
function computerName(code: number | null): string | null {
  if (code == null) return null;
  if (code === config.value.thisInput) return config.value.thisName?.trim() || "This PC";
  if (code === config.value.otherInput) return config.value.otherName?.trim() || "Other PC";
  return null;
}
/** The configured computers that exist on this monitor, in this/other order. */
const computers = computed(() =>
  [config.value.thisInput, config.value.otherInput]
    .filter((code): code is number => code != null && !!monitor.value?.inputs.some((i) => i.code === code))
    .filter((code, idx, all) => all.indexOf(code) === idx)
    .map((code) => ({ code, name: computerName(code)!, input: inputName(code) })),
);

async function attempt<T>(fn: () => Promise<T>): Promise<T | undefined> {
  error.value = "";
  busy.value = true;
  try {
    return await fn();
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function loadMonitors(refresh = false) {
  const list = await attempt(() => invoke<MonitorStatus[]>("get_monitors", { refresh }));
  if (list) monitors.value = list;
}

async function save() {
  await attempt(() => invoke("save_config", { config: config.value }));
}

async function switchTo(input: number) {
  await attempt(() => invoke("switch_input", { input }));
  // Give the monitor a moment to settle before reading back.
  setTimeout(() => loadMonitors(), 1500);
}

// ---- USB mapping: edit → Save → confirm within N seconds or it reverts ----

/** Only inputs that have a computer assigned are worth showing. */
const kvmSlots = computed(() => monitor.value?.kvm?.slots.filter((s) => computerName(s.input)) ?? []);
const draft = ref<Record<number, number>>({});
const dirty = computed(() => kvmSlots.value.some((s) => draft.value[s.input] !== undefined && draft.value[s.input] !== s.port));
const pending = ref<{ token: number; remaining: number } | null>(null);
let countdown: ReturnType<typeof setInterval> | undefined;

function resetDraft() {
  draft.value = Object.fromEntries(kvmSlots.value.map((s) => [s.input, s.port]));
}
watch(kvmSlots, resetDraft, { immediate: true });

function stopCountdown() {
  clearInterval(countdown);
  pending.value = null;
}

async function saveKvm() {
  if (!monitor.value) return;
  const changes = kvmSlots.value
    .filter((s) => draft.value[s.input] !== s.port)
    .map((s) => ({ input: s.input, port: draft.value[s.input] }));
  const res = await attempt(() =>
    invoke<{ token: number; seconds: number; kvm: KvmState }>("apply_kvm_ports", { monitor: monitor.value!.key, changes }),
  );
  if (!res) return;
  monitor.value.kvm = res.kvm;
  notice.value = "";
  pending.value = { token: res.token, remaining: res.seconds };
  clearInterval(countdown);
  countdown = setInterval(() => {
    if (pending.value && pending.value.remaining > 1) pending.value.remaining--;
  }, 1000);
}

async function keepKvm() {
  if (!pending.value) return;
  const kept = await attempt(() => invoke<boolean>("confirm_kvm", { token: pending.value!.token }));
  stopCountdown();
  notice.value = kept ? "USB mapping saved." : "Too late — the change was already reverted.";
}

async function revertKvm() {
  if (!pending.value) return;
  const kvm = await attempt(() => invoke<KvmState | null>("revert_kvm", { token: pending.value!.token }));
  if (kvm && monitor.value) monitor.value.kvm = kvm;
  stopCountdown();
  notice.value = "USB mapping reverted.";
}

// The backend owns the timer, so a revert also lands here if the window was hidden.
let unlistenRevert: UnlistenFn | undefined;
listen<KvmState>("kvm-reverted", (e) => {
  if (monitor.value) monitor.value.kvm = e.payload;
  stopCountdown();
  notice.value = "Not confirmed in time — USB mapping reverted.";
}).then((u) => (unlistenRevert = u));
onUnmounted(() => {
  unlistenRevert?.();
  clearInterval(countdown);
});

const platform = ref<{ os: string; wayland: boolean }>({ os: "", wayland: false });

function captureHotkey(e: KeyboardEvent) {
  e.preventDefault();
  if (e.key === "Escape") return void (capturing.value = false);
  if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return;
  const parts = [];
  if (e.ctrlKey) parts.push("Control");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  if (e.metaKey) parts.push("Super");
  if (parts.length === 0 && !/^F\d+$/.test(e.code)) {
    error.value = "Use at least one modifier (Ctrl / Alt / Shift / Win) unless it's a function key.";
    return;
  }
  parts.push(e.code);
  config.value.hotkey = parts.join("+");
  capturing.value = false;
  save();
}

function clearHotkey() {
  config.value.hotkey = null;
  save();
}

async function toggleAutostart() {
  await attempt(() => invoke("set_autostart", { enabled: autostart.value }));
}

watch(() => [config.value.monitor, config.value.thisInput, config.value.otherInput], () => {
  if (!loading.value) save();
});

onMounted(async () => {
  const [cfg, auto, plat] = await Promise.all([
    invoke<Config>("get_config"),
    invoke<boolean>("get_autostart").catch(() => false),
    invoke<{ os: string; wayland: boolean }>("platform_info"),
  ]);
  config.value = cfg;
  autostart.value = auto;
  platform.value = plat;
  await loadMonitors();
  // First run: assume the monitor's current input is this computer.
  if (config.value.thisInput == null && monitor.value?.current_input != null) {
    config.value.thisInput = monitor.value.current_input;
    await save();
  }
  loading.value = false;
});
</script>

<template>
  <main :class="{ busy }">
    <header>
      <h1>montools</h1>
      <button class="ghost" :disabled="busy" @click="loadMonitors(true)" title="Re-detect monitors">↻ Refresh</button>
    </header>

    <p v-if="error" class="banner error" @click="error = ''">{{ error }}</p>
    <p v-else-if="notice" class="banner ok" @click="notice = ''">{{ notice }}</p>

    <p v-if="loading" class="muted">Talking to monitors over DDC/CI…</p>

    <template v-else>
      <section>
        <h2>Monitor</h2>
        <select v-model="config.monitor">
          <option v-for="m in visibleMonitors" :key="m.key" :value="m.key">
            {{ m.key }}{{ m.kvm ? " · KVM" : "" }}
          </option>
        </select>
        <label v-if="hasKvm" class="row check small">
          <input type="checkbox" v-model="config.showAllMonitors" @change="save" />
          Show monitors without a KVM<span v-if="hiddenCount" class="muted">&nbsp;({{ hiddenCount }} hidden)</span>
        </label>
        <p v-if="!monitors.length" class="muted">
          No DDC/CI monitors found. Check that DDC/CI is enabled in the monitor's menu
          (on Linux: <code>i2c-dev</code> loaded and your user in the <code>i2c</code> group).
        </p>
        <div v-if="monitor" class="now">
          <div>
            <span class="muted">Showing</span>
            <strong>{{ inputName(monitor.current_input) }}</strong>
            <span v-if="computerName(monitor.current_input)" class="pill">{{ computerName(monitor.current_input) }}</span>
          </div>
        </div>
      </section>

      <section v-if="monitor && computers.length">
        <h2>Switch to</h2>
        <div class="computers">
          <button
            v-for="c in computers"
            :key="c.code"
            class="computer"
            :class="{ active: c.code === monitor.current_input }"
            :disabled="busy || c.code === monitor.current_input"
            @click="switchTo(c.code)"
          >
            <strong>{{ c.name }}</strong>
            <span>{{ c.code === monitor.current_input ? "Active" : c.input }}</span>
          </button>
        </div>
      </section>

      <section v-if="monitor">
        <h2>Computers</h2>
        <div class="grid">
          <label>This PC
            <input v-model="config.thisName" placeholder="Name, e.g. Desktop" maxlength="32" @change="save" />
          </label>
          <label>is on
            <select v-model="config.thisInput">
              <option :value="null">—</option>
              <option v-for="i in monitor.inputs" :key="i.code" :value="i.code">{{ i.name }}</option>
            </select>
          </label>
          <label>Other PC
            <input v-model="config.otherName" placeholder="Name, e.g. Work laptop" maxlength="32" @change="save" />
          </label>
          <label>is on
            <select v-model="config.otherInput">
              <option :value="null">—</option>
              <option v-for="i in monitor.inputs" :key="i.code" :value="i.code">{{ i.name }}</option>
            </select>
          </label>
        </div>
      </section>

      <section v-if="monitor?.kvm">
        <h2>USB / KVM mapping</h2>
        <p class="muted">
          Which USB upstream port gets your keyboard &amp; mouse for each video input.
          Stored in the monitor itself, so it applies no matter which computer switches.
        </p>
        <p v-if="!kvmSlots.length" class="muted small">Assign inputs to your computers above to set their USB ports.</p>
        <table v-else>
          <tr v-for="slot in kvmSlots" :key="slot.input">
            <td>
              {{ computerName(slot.input) }}
              <span class="muted">· {{ slot.input_name }}</span>
            </td>
            <td>
              <select v-model.number="draft[slot.input]" :disabled="busy || !!pending">
                <option v-for="p in monitor.kvm.ports" :key="p.port" :value="p.port">{{ p.name }}</option>
              </select>
            </td>
          </tr>
        </table>

        <div v-if="pending" class="confirm">
          <span>Keyboard &amp; mouse still work? Reverting in <strong>{{ pending.remaining }}</strong>s…</span>
          <button class="primary" @click="keepKvm">Keep</button>
          <button @click="revertKvm">Revert</button>
        </div>
        <div v-else-if="kvmSlots.length" class="row actions">
          <span class="muted small">Changes revert after 5s unless you confirm them.</span>
          <button :disabled="!dirty || busy" @click="resetDraft">Cancel</button>
          <button class="primary" :disabled="!dirty || busy" @click="saveKvm">Save</button>
        </div>
      </section>

      <section>
        <h2>Shortcut &amp; startup</h2>
        <div class="row">
          <span>Toggle hotkey</span>
          <button v-if="capturing" class="capture" autofocus @keydown="captureHotkey" @blur="capturing = false">
            Press keys… (Esc cancels)
          </button>
          <button v-else class="ghost" @click="capturing = true">
            {{ config.hotkey ? config.hotkey.replace(/Key|Digit/g, "") : "Set hotkey" }}
          </button>
          <button v-if="config.hotkey && !capturing" class="ghost" @click="clearHotkey">✕</button>
        </div>
        <p v-if="platform.wayland" class="muted small">
          Wayland doesn't let apps register global hotkeys, so this may not fire. Instead, add a custom
          shortcut in your desktop's keyboard settings that runs <code>montools-cli switch toggle</code>.
        </p>
        <label class="row check">
          <input type="checkbox" v-model="autostart" @change="toggleAutostart" />
          Start montools when I log in
        </label>
      </section>

      <p class="muted small footer">Closing this window keeps montools running in the tray.</p>
    </template>
  </main>
</template>

<style>
:root {
  --bg: #f6f7f9; --card: #fff; --fg: #1d2129; --muted: #697386; --line: #e3e6eb;
  --accent: #2f6fed; --accent-fg: #fff; --err: #b42318; --err-bg: #fdecea; --ok: #1a7f37; --ok-bg: #e6f4ea;
  font-family: system-ui, -apple-system, "Segoe UI", sans-serif; font-size: 14px;
  color: var(--fg); background: var(--bg);
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #16181d; --card: #1f2229; --fg: #e6e8ec; --muted: #9aa3b2; --line: #2e323b;
    --accent: #5b8def; --err: #ff8a80; --err-bg: #3a1d1b; --ok: #7bd389; --ok-bg: #18301f;
  }
}
body { margin: 0; background: var(--bg); }
main { padding: 16px 20px 24px; max-width: 640px; margin: 0 auto; }
main.busy { cursor: progress; }
header { display: flex; align-items: center; justify-content: space-between; }
h1 { font-size: 18px; margin: 0; }
h2 { font-size: 12px; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); margin: 0 0 10px; }
section { background: var(--card); border: 1px solid var(--line); border-radius: 10px; padding: 14px 16px; margin-top: 14px; }
.muted { color: var(--muted); }
.small { font-size: 12px; margin-bottom: 0; }
select, button, input { font: inherit; color: inherit; }
input:not([type]) { background: var(--card); border: 1px solid var(--line); border-radius: 6px; padding: 6px 8px; }
select { background: var(--card); border: 1px solid var(--line); border-radius: 6px; padding: 6px 8px; width: 100%; }
button { border-radius: 6px; padding: 6px 12px; border: 1px solid var(--line); background: var(--card); cursor: pointer; }
button:disabled { opacity: .5; cursor: default; }
button.primary { background: var(--accent); color: var(--accent-fg); border-color: var(--accent); font-weight: 600; }
button.ghost { background: transparent; }
button.capture { border-color: var(--accent); color: var(--accent); }
.now { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 12px; }
.now strong { margin: 0 6px; font-size: 16px; }
.pill { font-size: 11px; border: 1px solid var(--line); border-radius: 999px; padding: 1px 8px; color: var(--muted); margin-left: 4px; }
.grid { display: grid; grid-template-columns: 3fr 2fr; gap: 10px 12px; }
.grid label { display: flex; flex-direction: column; gap: 4px; color: var(--muted); }
.computers { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 10px; }
.computer { display: flex; flex-direction: column; align-items: flex-start; gap: 2px; padding: 12px 14px; text-align: left; }
.computer strong { font-size: 15px; }
.computer span { font-size: 12px; color: var(--muted); }
.computer:not(:disabled):hover { border-color: var(--accent); }
.computer.active { opacity: 1; border-color: var(--accent); background: color-mix(in srgb, var(--accent) 12%, var(--card)); }
.computer.active span { color: var(--accent); font-weight: 600; }
table { width: 100%; border-collapse: collapse; }
td { padding: 6px 0; border-top: 1px solid var(--line); }
td:last-child { width: 45%; }
tr:first-child td { border-top: none; }
.row { display: flex; align-items: center; gap: 8px; }
.row > span { flex: 1; }
.check { margin-top: 12px; cursor: pointer; }
.banner { padding: 8px 12px; border-radius: 8px; margin: 12px 0 0; cursor: pointer; }
.banner.error { background: var(--err-bg); color: var(--err); }
.banner.ok { background: var(--ok-bg); color: var(--ok); }
.footer { text-align: center; margin-top: 16px; }
.actions { margin-top: 12px; }
.confirm { display: flex; align-items: center; gap: 8px; margin-top: 12px; padding: 10px 12px; border-radius: 8px; border: 1px solid var(--accent); }
.confirm span { flex: 1; }
code { font-size: 12px; }
</style>
