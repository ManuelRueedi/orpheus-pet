import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { getVoices } from "./pet/orpheus.js";
import { LINES, VOICE_LANG } from "./pet/samples.js";

// This is the PANEL window: a thin control surface (voice/language pickers,
// text box, hotkey recorder, download UI). It owns no audio — it forwards
// commands to the pet window over events and displays the status the pet
// reports back. The pet positions and shows/hides this window.
const el = (id) => document.getElementById(id);
const win = getCurrentWindow();

// Map the pet's (text, kind) status into a visual state + clean label. The dot
// colour keys off the state class (see .status.state-* in styles.css).
function statusVisual(text, kind) {
  const t = (text || "").toLowerCase();
  if (kind === "error") return { state: "error", label: text || "Something went wrong" };
  if (t.includes("pause")) return { state: "paused", label: "Paused" };
  if (kind === "warn") return { state: "warn", label: text };
  if (t.includes("speak")) return { state: "reading", label: "Reading aloud" };
  if (kind === "busy") return { state: "thinking", label: text || "Working…" };
  return { state: "idle", label: !text || t === "idle" ? "Ready" : text };
}

function setStatus(text, kind = "") {
  const box = el("status");
  if (!box) return;
  const { state, label } = statusVisual(text, kind);
  const t = el("statusText");
  if (t) t.textContent = label;
  box.className = "status state-" + state;
}

const LANG_NAMES = {
  en: "English", fr: "Français", de: "Deutsch", es: "Español",
  it: "Italiano", ko: "한국어", hi: "हिन्दी", zh: "中文",
};
const LANG_ORDER = ["en", "fr", "de", "es", "it", "ko", "hi", "zh"];

let allVoices = [];
let currentLang = "en";
let availableLangs = [];
let switchingLang = false;

// Voice-model size presets (the config's `quant`). Smaller = faster + less
// VRAM/disk, at some quality cost. English publishes all three; other languages
// fall back to Q8_0 in the backend when a smaller quant isn't available.
// `vram` = rough VRAM (GB) to run the whole model on the GPU (optimal / -ngl 99).
const QUANTS = [
  { id: "Q8_0", label: "Best quality", vram: 6 },
  { id: "Q4_K_M", label: "Balanced", vram: 3 },
  { id: "Q2_K", label: "Low-spec", vram: 2 },
];
const quantLabel = (id) => (QUANTS.find((q) => q.id === id) || {}).label || id;
// Dropdown option text: quality label + the VRAM it wants for full-GPU performance.
const quantOptionText = (q) => `${q.label} · ~${q.vram} GB VRAM`;
// Approx download size per quant (bytes), mirroring the backend's est_size so the
// language dropdown's "⬇ X GB" tags match the confirm dialog. Unknown → Q8_0.
const QUANT_BYTES = { Q8_0: 3_516_430_784, Q4_K_M: 2_360_000_000, Q2_K: 1_600_000_000 };
const quantGb = (id) => ((QUANT_BYTES[id] || QUANT_BYTES.Q8_0) / 1e9).toFixed(1);
// Seed from the last local choice so size tags are right immediately; initQuantUi
// refines it from the backend (authoritative) once the engine answers.
let currentQuant = localStorage.getItem("pet.quant") || "Q8_0";

// Every voice maps to a language (a Korean voice → Korean lines, etc.).
function langForVoice(voice) {
  return VOICE_LANG[voice] || "en";
}

// Default language from Windows: navigator.languages is the OS "preferred
// languages" list (its first entry is the Windows display language). Pick the
// first one we support; otherwise English.
function detectLang(available) {
  const prefs =
    navigator.languages && navigator.languages.length
      ? navigator.languages
      : [navigator.language || "en"];
  for (const tag of prefs) {
    const primary = String(tag).toLowerCase().split("-")[0];
    if (available.includes(primary)) return primary;
  }
  return available.includes("en") ? "en" : available[0];
}

function voicesForLang(lang) {
  return allVoices.filter((v) => langForVoice(v) === lang);
}

// Fill the voice dropdown with just the chosen language's voices, keeping the
// saved voice if it belongs to that language, otherwise the first one.
function fillVoiceOptions(lang) {
  const sel = el("voice");
  const list = voicesForLang(lang);
  sel.innerHTML = "";
  for (const v of list) {
    const opt = document.createElement("option");
    opt.value = v;
    opt.textContent = v;
    sel.appendChild(opt);
  }
  const saved = localStorage.getItem("pet.voice");
  if (saved && list.includes(saved)) sel.value = saved;
  else if (list.length) sel.value = list[0];
}

// The currently active language = whichever model the engine actually has
// loaded (Rust get_language). Falls back to saved/Windows locale if the command
// isn't available (e.g. engine still booting).
async function currentLangFromBackend() {
  try {
    const l = await invoke("get_language");
    if (l) return l;
  } catch { /* command unavailable */ }
  return localStorage.getItem("pet.lang") || detectLang(Object.keys(LINES));
}

// (Re)build the language dropdown, tagging languages whose model isn't downloaded
// yet with the size for the CURRENTLY selected quality. Preserves the selection.
async function rebuildLangOptions() {
  let installed = null;
  try {
    installed = await invoke("installed_languages");
  } catch { /* leave null → don't tag anything */ }
  const notInstalled = (l) => Array.isArray(installed) && !installed.includes(l);
  const gb = quantGb(currentQuant);
  const sel = el("lang");
  const keep = sel.value;
  sel.innerHTML = "";
  for (const l of availableLangs) {
    const name = LANG_NAMES[l] || l;
    const opt = document.createElement("option");
    opt.value = l;
    // Don't tag the language you're currently using — it's loaded and usable even
    // if its model at the selected detail isn't downloaded.
    opt.textContent = notInstalled(l) && l !== currentLang ? `${name}  ⬇ ${gb} GB` : name;
    sel.appendChild(opt);
  }
  if (keep) sel.value = keep;
}

// Populate the language dropdown (only languages with voices), select the given
// one (the loaded model's language), and fill the matching voices.
async function setupLangAndVoices(voices, preferred) {
  allVoices = voices;
  availableLangs = LANG_ORDER.filter((l) => voices.some((v) => langForVoice(v) === l));
  currentLang =
    preferred && availableLangs.includes(preferred)
      ? preferred
      : availableLangs.includes("en") ? "en" : availableLangs[0];
  await rebuildLangOptions();
  el("lang").value = currentLang;
  localStorage.setItem("pet.lang", currentLang);
  fillVoiceOptions(currentLang);
}

// Tell the pet which voice is active. greet=false for the silent initial sync;
// greet=true when the user picks a new voice (the pet says a short hello).
function syncVoiceToPet(greet) {
  const v = el("voice").value;
  if (v) emitTo("main", "ui:voice", { voice: v, greet }).catch(() => {});
}

// The app itself spawns llama-server + Orpheus-FastAPI (src-tauri/src/stack.rs).
// The model load takes a few seconds, so poll until the engine answers.
async function populateVoices() {
  setStatus("starting voice engine…", "busy");
  const deadline = Date.now() + 150_000;
  while (Date.now() < deadline) {
    const { ok, voices } = await getVoices();
    if (ok) {
      await setupLangAndVoices(voices, await currentLangFromBackend());
      setStatus("idle");
      syncVoiceToPet(false);
      return;
    }
    await new Promise((r) => setTimeout(r, 2500));
  }
  const { voices } = await getVoices(); // falls back to the built-in list
  await setupLangAndVoices(voices, await currentLangFromBackend());
  setStatus("engine offline", "error");
  syncVoiceToPet(false);
  try {
    console.warn("stack status:", await invoke("stack_status"));
  } catch { /* command unavailable */ }
}

// Panel sub-states: idle (text box + Speak) / download-confirm / download-progress.
// The confirm and progress UIs take over the text box + Speak space; the pickers
// stay put above them.
function setPanelState(state) {
  const idle = state === "idle";
  el("text").style.display = idle ? "block" : "none";
  el("speakRow").style.display = idle ? "flex" : "none";
  el("confirmRow").style.display = state === "confirm" ? "flex" : "none";
  el("dlRow").style.display = state === "downloading" ? "flex" : "none";
}
function setDownloadProgress(pct, label) {
  const f = el("dlFill");
  if (f) f.style.width = `${Math.max(0, Math.min(100, pct))}%`;
  const l = el("dlLabel");
  if (l && label) l.textContent = label;
}

// When a switch needs a download we show a confirm first; these hold the action
// to run on "Download" and how to revert the dropdown on "Cancel". Shared by the
// language and model-size flows.
let pendingAction = null;
let pendingRevert = null;

// Re-sync the dropdowns to the model the engine actually has loaded. Used after a
// cancelled or failed switch so the UI never claims a language/size the backend
// isn't in (setting .value programmatically doesn't fire the change listeners).
async function syncActiveState() {
  try {
    const lang = await invoke("get_language");
    if (lang) {
      currentLang = lang;
      el("lang").value = lang;
      fillVoiceOptions(lang);
    }
  } catch { /* backend unavailable — leave as-is */ }
  try {
    const quant = await invoke("get_quant");
    if (quant) {
      currentQuant = quant;
      if ([...el("quality").options].some((o) => o.value === quant)) {
        el("quality").value = quant;
      }
    }
  } catch { /* backend unavailable */ }
  await rebuildLangOptions();
}

// Each language is a SEPARATE Orpheus model. If it isn't downloaded yet, confirm
// the download first; otherwise switch straight away.
async function onLangChange(target) {
  if (switchingLang || target === currentLang) return;
  const prev = currentLang;
  let status;
  try {
    status = await invoke("model_status", { lang: target });
  } catch {
    status = { present: true }; // assume present if the check fails
  }
  if (status.present) {
    doSwitch(target);
    return;
  }
  const gb = (Number(status.sizeBytes || 0) / 1e9).toFixed(1);
  pendingAction = () => doSwitch(target);
  pendingRevert = () => { el("lang").value = prev; };
  el("confirmMsg").textContent =
    `Download the ${LANG_NAMES[target] || target} model (~${gb} GB)?`;
  setPanelState("confirm");
}

// Download (if needed) + hot-swap to `target`, gating the pet's speech while it
// happens; on failure or cancel, resync the dropdowns to the loaded model.
async function doSwitch(target) {
  if (switchingLang) return;
  switchingLang = true;
  emitTo("main", "ui:switching", { active: true }).catch(() => {});
  el("speak").disabled = true;
  setStatus(`preparing ${LANG_NAMES[target] || target}…`, "busy");
  try {
    await invoke("set_language", { lang: target });
    currentLang = target;
    localStorage.setItem("pet.lang", target);
    fillVoiceOptions(target);
    await rebuildLangOptions(); // the just-installed language loses its download tag
    const v = el("voice").value;
    localStorage.setItem("pet.voice", v);
    setStatus("idle");
    switchingLang = false;
    // Ungate the pet BEFORE the greeting, then have it greet in the new voice.
    emitTo("main", "ui:switching", { active: false }).catch(() => {});
    emitTo("main", "ui:voice", { voice: v, greet: true }).catch(() => {});
  } catch (e) {
    const msg = String(e?.message || e);
    if (msg.includes("cancelled")) setStatus("download cancelled", "warn");
    else {
      console.error(e);
      setStatus(`couldn't switch: ${msg.slice(0, 46)}`, "error");
    }
    switchingLang = false;
    emitTo("main", "ui:switching", { active: false }).catch(() => {});
    await syncActiveState(); // dropdowns back to the model that's actually loaded
  } finally {
    setPanelState("idle");
    el("speak").disabled = false;
  }
}

// Detail (model size) is a download PREFERENCE, independent of the current voice.
// Changing it never downloads: it records the size, refreshes the language list to
// show that size, and — only if the current language already has that size on disk
// — hot-swaps to it. Otherwise the current voice keeps playing and the new size
// applies the next time a language is picked.
async function onQuantChange(target) {
  if (switchingLang || target === currentQuant) return;
  switchingLang = true; // set before the first await so a double-select can't re-enter
  el("speak").disabled = true;
  // Will applying this reload the current voice (its model at this size is on
  // disk) or just set the preference (not downloaded → no reload, no download)?
  let willLoad = false;
  try {
    const st = await invoke("model_status", { lang: currentLang, quant: target });
    willLoad = !!st.present;
  } catch { /* treat as preference-only */ }
  if (willLoad) {
    emitTo("main", "ui:switching", { active: true }).catch(() => {});
    setStatus(`switching to ${quantLabel(target)}…`, "busy");
  }
  try {
    await invoke("set_quant", { quant: target });
    currentQuant = target;
    localStorage.setItem("pet.quant", target);
    await rebuildLangOptions(); // language sizes/tags now reflect the new detail
    setStatus(willLoad ? "idle" : `${quantLabel(target)} — used for new downloads`);
  } catch (e) {
    console.error(e);
    setStatus(`couldn't apply: ${String(e?.message || e).slice(0, 40)}`, "error");
    await syncActiveState(); // put the dropdowns back to the real state
  } finally {
    switchingLang = false;
    el("speak").disabled = false;
    if (willLoad) emitTo("main", "ui:switching", { active: false }).catch(() => {});
    setPanelState("idle");
  }
}

function requestSpeak() {
  const text = el("text").value.trim();
  if (!text) { el("text").focus(); return; }
  emitTo("main", "ui:speak", { text, voice: el("voice").value }).catch(() => {});
}

function wireUi() {
  el("speak").addEventListener("click", requestSpeak);
  el("hide").addEventListener("click", () => emitTo("main", "ui:hide", {}).catch(() => {}));
  el("voice").addEventListener("change", () => {
    const v = el("voice").value;
    localStorage.setItem("pet.voice", v);
    syncVoiceToPet(true); // pet says a short hello in the newly picked voice
  });
  el("lang").addEventListener("change", () => onLangChange(el("lang").value));
  el("quality").addEventListener("change", () => onQuantChange(el("quality").value));
  el("confirmDl").addEventListener("click", () => {
    el("stopDl").disabled = false;
    setPanelState("downloading");
    setDownloadProgress(0, "Downloading…");
    if (pendingAction) pendingAction();
  });
  el("confirmCancel").addEventListener("click", () => {
    if (pendingRevert) pendingRevert(); // undo the dropdown change
    setPanelState("idle");
  });
  el("stopDl").addEventListener("click", () => {
    el("stopDl").disabled = true;
    el("dlLabel").textContent = "stopping…";
    invoke("cancel_download").catch(() => {});
  });

  el("text").addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      requestSpeak();
    } else if (e.key === "Escape") {
      emitTo("main", "ui:close", {}).catch(() => {});
    }
  });

  // Feel native: suppress the browser context menu and stray text selection
  // everywhere except the actual form controls.
  window.addEventListener("contextmenu", (e) => {
    if (!e.target.closest("input, textarea, select")) e.preventDefault();
  });
}

// ---- Hotkey recorder ----------------------------------------------------
// Turns a KeyboardEvent.code + modifiers into the combo string Rust expects
// (e.g. "ctrl+alt+KeyS") and a friendly label (e.g. "Ctrl+Alt+S").
function keyLabel(tok) {
  const t = tok.trim();
  let m;
  if ((m = /^Key([A-Za-z])$/.exec(t))) return m[1].toUpperCase();
  if ((m = /^Digit(\d)$/.exec(t))) return m[1];
  if (/^F\d{1,2}$/i.test(t)) return t.toUpperCase();
  if (t.length === 1) return t.toUpperCase();
  const map = {
    Backquote: "`", Minus: "-", Equal: "=", Space: "Space", Backslash: "\\",
    BracketLeft: "[", BracketRight: "]", Semicolon: ";", Quote: "'",
    Comma: ",", Period: ".", Slash: "/",
  };
  return map[t] || t;
}
function modLabel(tok) {
  const t = tok.toLowerCase();
  if (t === "ctrl" || t === "control") return "Ctrl";
  if (t === "alt" || t === "option") return "Alt";
  if (t === "shift") return "Shift";
  if (["super", "meta", "cmd", "command", "win"].includes(t)) return "Win";
  return null;
}
function comboToLabel(combo) {
  return combo
    .split("+")
    .map((tok) => modLabel(tok) || keyLabel(tok))
    .join("+");
}

const MOD_CODES = new Set([
  "ControlLeft", "ControlRight", "AltLeft", "AltRight",
  "ShiftLeft", "ShiftRight", "MetaLeft", "MetaRight",
]);

async function initHotkeyUi() {
  const btn = el("hotkey");
  let current = "ctrl+alt+s";
  try { current = await invoke("get_hotkey"); } catch { /* command missing */ }
  btn.textContent = comboToLabel(current);

  let recording = false;
  const stop = () => {
    recording = false;
    btn.classList.remove("recording");
    window.removeEventListener("keydown", onKey, true);
  };

  const onKey = async (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") { stop(); btn.textContent = comboToLabel(current); return; }

    const mods = [];
    if (e.ctrlKey) mods.push("ctrl");
    if (e.altKey) mods.push("alt");
    if (e.shiftKey) mods.push("shift");
    if (e.metaKey) mods.push("super");

    if (MOD_CODES.has(e.code)) {
      // Still holding only modifiers — show a live preview, keep listening.
      btn.textContent = mods.map(modLabel).join("+") + "+…";
      return;
    }
    const isFn = /^F\d{1,2}$/.test(e.code);
    if (mods.length === 0 && !isFn) {
      btn.textContent = "add a modifier…";
      return;
    }

    const combo = [...mods, e.code].join("+");
    stop();
    btn.textContent = "saving…";
    try {
      const applied = await invoke("set_hotkey", { combo });
      current = applied;
      btn.textContent = comboToLabel(applied);
      btn.classList.add("saved");
      setTimeout(() => btn.classList.remove("saved"), 1200);
      setStatus("hotkey updated", "warn");
    } catch (err) {
      console.error(err);
      btn.textContent = comboToLabel(current);
      setStatus("that combo isn't supported", "error");
    }
  };

  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    if (recording) { stop(); btn.textContent = comboToLabel(current); return; }
    recording = true;
    btn.classList.add("recording");
    btn.textContent = "press keys…";
    window.addEventListener("keydown", onKey, true);
  });
  btn.addEventListener("blur", () => {
    if (recording) { stop(); btn.textContent = comboToLabel(current); }
  });
}

// Populate the model-size dropdown and select the active quant (from the backend
// config, falling back to the last local choice).
async function initQuantUi() {
  const sel = el("quality");
  if (!sel) return;
  // Populate synchronously from the presets so the dropdown is usable immediately
  // (currentQuant is already seeded from localStorage).
  sel.innerHTML = "";
  for (const q of QUANTS) {
    const opt = document.createElement("option");
    opt.value = q.id;
    opt.textContent = quantOptionText(q);
    sel.appendChild(opt);
  }
  sel.value = currentQuant;
  // Then refine from the backend (authoritative). Keep the seeded value if it fails.
  try {
    const q = await invoke("get_quant");
    if (q) {
      currentQuant = q;
      // A config quant outside our presets (e.g. Q5_K_M) still shows honestly.
      if (!QUANTS.some((x) => x.id === q)) {
        const opt = document.createElement("option");
        opt.value = q;
        opt.textContent = q;
        sel.appendChild(opt);
      }
      sel.value = q;
    }
  } catch { /* keep the seeded value */ }
}

async function main() {
  wireUi();
  initHotkeyUi();
  // Fire-and-forget (don't block the panel on it): currentQuant is already seeded
  // from localStorage, and the engine boot that gates the language list is far
  // slower than this get_quant, so the size tags render correctly regardless.
  initQuantUi();

  // Focus the text box whenever the panel gains focus (the pet focuses this
  // window when it opens the panel).
  win.onFocusChanged(({ payload }) => {
    if (payload) el("text").focus();
  }).catch(() => {});

  // Status updates from the pet (thinking / speaking / errors).
  await listen("pet:status", (e) => {
    const p = e.payload || {};
    setStatus(p.text, p.kind);
  });
  // The pet just opened us → focus the text box.
  await listen("pet:panel-open", () => el("text").focus());
  // The pet (re)connected → resync the active voice so it can speak samples.
  await listen("pet:ready", () => {
    if (allVoices.length) syncVoiceToPet(false);
  });

  // Model download / load progress → the in-panel progress bar (the pet shows
  // the cauldron from the same event).
  await listen("model-progress", (e) => {
    const p = e.payload || {};
    const name = LANG_NAMES[p.lang] || p.lang || "";
    const gb = (n) => (Number(n || 0) / 1e9).toFixed(1);
    if (p.phase === "download") {
      const pct = p.pct ?? 0;
      setPanelState("downloading");
      const detail = p.total ? `${pct}% · ${gb(p.received)}/${gb(p.total)} GB` : `${gb(p.received)} GB`;
      setDownloadProgress(pct, `Downloading ${name} · ${detail}`);
    } else if (p.phase === "loading") {
      setPanelState("downloading");
      setDownloadProgress(100, `Loading ${name} model…`);
      el("stopDl").disabled = true; // download's done; nothing left to stop
    }
  });

  // Deliberately not awaited: the UI stays usable while the engine boots.
  populateVoices();
}

window.addEventListener("DOMContentLoaded", main);
