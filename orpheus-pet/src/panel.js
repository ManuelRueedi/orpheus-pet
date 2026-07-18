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
let loadedLang = null;
let availableLangs = [];
let switchingLang = false;
let engineReady = false;
let modelManagementReady = false;
let currentStackState = "starting-backend";
let panelState = "idle";
let voicesAreLive = false;
let readinessPoll = 0;
let readinessRevision = 0;
let readinessStateSince = Date.now();
let operationError = "";
let operationRetryKind = "model";
let retryLanguage = null;
let retryQuant = null;
let runtimeInstallPlan = null;
let runtimeInstallPlanLoading = false;
let installingRuntime = false;
let backendModelOperationActive = false;
let probingSelection = false;
let selectionRevision = 0;
let langOptionsRevision = 0;
let operationSequence = 0;
let activeOperationId = null;

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
let loadedQuant = null;
let stagedVoice = null;

function newOperationId(kind) {
  operationSequence += 1;
  const suffix = globalThis.crypto?.randomUUID?.() || `${Date.now()}-${operationSequence}`;
  return `${kind}-${suffix}`;
}

function ensureQuantOption(quant) {
  const sel = el("quality");
  if (!sel || !quant || [...sel.options].some((o) => o.value === quant)) return;
  const opt = document.createElement("option");
  opt.value = quant;
  opt.textContent = quantLabel(quant);
  sel.appendChild(opt);
}

function updatePickerGate() {
  // Availability probes and confirmations are staged UI, not active model work:
  // keep both selectors editable so language + size can be composed either way.
  const busy = switchingLang || installingRuntime || backendModelOperationActive;
  el("lang").disabled = !modelManagementReady || busy;
  el("quality").disabled = !modelManagementReady || busy;
}

function hasStagedModelSelection() {
  return currentLang !== loadedLang || currentQuant !== loadedQuant ||
    pendingAction?.kind === "model-selection" || probingSelection;
}

function publishSwitching(active, operationId) {
  emitTo("main", "ui:switching", { active, operationId }).catch(() => {});
}

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

// Prefer the language the engine actually loaded (Rust get_language). With no
// model yet, fall back to the saved/Windows locale as the initial staged choice.
async function currentLangFromBackend() {
  try {
    const l = await invoke("get_language");
    if (l) return l;
  } catch { /* command unavailable */ }
  return localStorage.getItem("pet.lang") || detectLang(Object.keys(LINES));
}

// (Re)build the language dropdown, tagging languages whose model isn't downloaded
// yet with the size for the CURRENTLY selected quality. Preserves the selection.
async function rebuildLangOptions({ quant = currentQuant } = {}) {
  const revision = ++langOptionsRevision;
  const selectedQuant = quant;
  let installed = null;
  try {
    installed = await invoke("installed_languages", { quant: selectedQuant });
  } catch { /* leave null → don't tag anything */ }
  // Polling and selection changes can rebuild concurrently. Never let a result
  // fetched for an older size overwrite the latest selection.
  if (revision !== langOptionsRevision || selectedQuant !== currentQuant) return false;
  const availabilityKnown = Array.isArray(installed);
  const notInstalled = (l) => availabilityKnown && !installed.includes(l);
  const gb = quantGb(selectedQuant);
  const sel = el("lang");
  const keep = sel.value;
  sel.innerHTML = "";
  for (const l of availableLangs) {
    const name = LANG_NAMES[l] || l;
    const opt = document.createElement("option");
    opt.value = l;
    const targetDiffers = currentLang !== loadedLang || selectedQuant !== loadedQuant;
    const activeSize = l === loadedLang && loadedQuant && targetDiffers
      ? ` · ${quantLabel(loadedQuant)} active`
      : "";
    // Availability always describes the selected size. An older loaded size can
    // keep speaking, but must not make a missing selected-size model look present.
    opt.textContent = !availabilityKnown
      ? `${name}${activeSize} · availability unknown`
      : notInstalled(l)
        ? `${name}${activeSize}  ⬇ ${gb} GB`
        : `${name}${activeSize}`;
    sel.appendChild(opt);
  }
  if (keep && availableLangs.includes(keep)) sel.value = keep;
  else if (availableLangs.includes(currentLang)) sel.value = currentLang;
  return true;
}

// Populate the language dropdown (only languages with voices), select the
// loaded language or initial locale preference, and fill the matching voices.
async function setupLangAndVoices(voices, preferred) {
  const revision = selectionRevision;
  allVoices = voices;
  availableLangs = LANG_ORDER.filter((l) => voices.some((v) => langForVoice(v) === l));
  currentLang =
    preferred && availableLangs.includes(preferred)
      ? preferred
      : availableLangs.includes("en") ? "en" : availableLangs[0];
  await rebuildLangOptions();
  if (revision !== selectionRevision || pendingAction || probingSelection || switchingLang) return;
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

// /voices only proves that the lightweight FastAPI process is listening. It can
// answer while llama-server or its model is absent, so stack_status is the sole
// source of truth for whether speech may run.
function normalizeStackStatus(raw, model) {
  raw = raw && typeof raw === "object" ? raw : {};
  const notes = Array.isArray(raw.notes) ? raw.notes.map(String) : [];
  const configReady = raw.configReady ?? !notes.some((n) => /config error/i.test(n));
  const runtimePresent = raw.runtimePresent ?? true;
  const backendPresent = raw.backendPresent ?? true;
  const backendReady = raw.backendReady ?? raw.orpheusUp ?? false;
  const llamaReady = raw.llamaReady ?? raw.llamaUp ?? false;
  const llamaReused = !!raw.llamaReused;
  const portConflict = !!(raw.portConflict?.llama || raw.portConflict?.backend);
  const modelPresent = raw.modelPresent ?? model?.present ?? false;
  const ready = raw.ready ?? (backendReady && llamaReady && modelPresent);
  const modelOperation = raw.modelOperation || null;
  const modelOperationActive = typeof modelOperation === "object"
    ? !!modelOperation?.active
    : !!modelOperation;

  let state = raw.state;
  if (!state) {
    if (!configReady) state = "config-error";
    else if (!runtimePresent) state = "runtime-missing";
    else if (!backendPresent) state = "backend-missing";
    else if (portConflict) state = "port-conflict";
    else if (!modelPresent) state = "model-missing";
    else if (!backendReady) state = "starting-backend";
    else if (!llamaReady) state = "loading-model";
    else state = ready ? "ready" : "error";
  }
  return {
    ...raw,
    configReady,
    runtimePresent,
    backendPresent,
    backendReady,
    llamaReady,
    llamaReused,
    portConflict: raw.portConflict || { llama: false, backend: false },
    modelPresent,
    modelOperation,
    modelOperationActive,
    preferredQuant: raw.preferredQuant || null,
    loadedQuant: raw.loadedQuant || null,
    ready: !!ready && state === "ready",
    state,
    notes,
    modelSizeBytes: Number(model?.sizeBytes || 0),
  };
}

function setupCopy(stack) {
  const operation = stack.modelOperation?.active ? stack.modelOperation : null;
  const displayLang = operation?.targetLanguage || currentLang;
  const displayQuant = operation?.targetQuant || currentQuant;
  const langName = LANG_NAMES[displayLang] || displayLang || "voice";
  const modelBytes = stack.modelSizeBytes || QUANT_BYTES[displayQuant] || QUANT_BYTES.Q8_0;
  const gb = (modelBytes / 1e9).toFixed(1);
  const stateAge = stack.state === currentStackState ? Date.now() - readinessStateSince : 0;
  const canRetryModel = stack.configReady && stack.runtimePresent && stack.backendPresent;
  const modelRecoveryState = ["model-missing", "loading-model", "error"].includes(stack.state);
  const runtimeRecoveryState = ["runtime-missing", "backend-missing", "runtime-installing"].includes(stack.state);
  if (operationError && (
    operationRetryKind === "retry" ||
    (operationRetryKind === "runtime" && runtimeRecoveryState) ||
    (canRetryModel && modelRecoveryState)
  )) {
    const retryName = LANG_NAMES[retryLanguage] || retryLanguage || langName;
    const retryLabel = operationRetryKind === "model"
      ? `Try ${retryName} again`
      : operationRetryKind === "quant"
        ? `Try ${quantLabel(retryQuant)} again`
        : operationRetryKind === "runtime"
          ? "Try runtime install again"
          : "Restart again";
    return {
      title: "Speech setup failed",
      message: operationError,
      hint: "Try again; if it still fails, restart Orpheus Pet and inspect its log directory.",
      action: operationRetryKind,
      actionLabel: retryLabel,
      autoOpen: true,
      kind: "error",
    };
  }
  switch (stack.state) {
    case "config-error":
      return {
        title: "Configuration needs repair",
        message: "The speech stack configuration could not be loaded.",
        hint: "Restart the app. From source, rerun the setup script if this persists.",
        action: "retry",
        actionLabel: "Check again",
        autoOpen: true,
        kind: "error",
      };
    case "runtime-missing":
    case "backend-missing": {
      const canInstall = !!runtimeInstallPlan?.available;
      const canRetryInstall = !!runtimeInstallPlan?.error &&
        !/disabled in development/i.test(runtimeInstallPlan.error);
      const flavor = runtimeInstallPlan?.flavor === "cpu" ? "CPU" : "NVIDIA GPU";
      const size = runtimeInstallPlan?.approximateBytes
        ? ` (~${(Number(runtimeInstallPlan.approximateBytes) / 1e9).toFixed(1)} GB)`
        : "";
      return {
        title: "Runtime pack not installed",
        message: "Orpheus needs its local speech runtime before it can talk.",
        hint: canInstall
          ? `Install the verified ${flavor} runtime once${size}; voice models remain separate.`
          : "From source, run the project setup script. Release builds can install the runtime here once its asset is published.",
        action: canInstall || canRetryInstall ? "runtime" : "retry",
        actionLabel: canInstall
          ? `Install ${flavor} runtime`
          : canRetryInstall ? "Check runtime download" : "Check again",
        autoOpen: true,
        kind: canInstall ? "warn" : "error",
      };
    }
    case "runtime-installing":
      return {
        title: "Installing speech runtime",
        message: "Downloading and verifying the local speech engine…",
        hint: "The app verifies every file before activating it.",
        action: null,
        actionLabel: "",
        autoOpen: true,
        kind: "busy",
      };
    case "port-conflict": {
      const ports = [];
      if (stack.portConflict?.llama) ports.push(stack.ports?.llama || 1234);
      if (stack.portConflict?.backend) ports.push(stack.ports?.backend || 5005);
      return {
        title: "Speech port is already in use",
        message: `Another process is blocking local speech${ports.length ? ` on port ${ports.join(" / ")}` : ""}.`,
        hint: "Close that process, then retry. Orpheus will only reuse a service that passes its health check.",
        action: "retry",
        actionLabel: "Check again",
        autoOpen: true,
        kind: "error",
      };
    }
    case "model-missing":
      return {
        title: "One download to start",
        message: `Download the ${langName} voice model (~${gb} GB).`,
        hint: "You can choose a smaller model size below before downloading.",
        action: "model",
        actionLabel: `Download ${langName}`,
        autoOpen: true,
        kind: "warn",
      };
    case "loading-model":
      return {
        title: stateAge > 150_000 ? "Voice model needs attention" : "Loading voice model",
        message: stateAge > 150_000
          ? `${langName} is taking longer than expected to load.`
          : `${langName} is being prepared for speech.`,
        hint: stateAge > 150_000
          ? "Try again, then restart the app or check llama-server.log."
          : "This can take a minute on the first launch.",
        action: stateAge > 150_000 ? "retry" : null,
        actionLabel: "Try again",
        autoOpen: stateAge > 150_000,
        kind: stateAge > 150_000 ? "error" : "busy",
      };
    case "starting-backend": {
      // The backend's first import may populate its local decoder cache, so give
      // it the same generous window as a model load before declaring recovery.
      const timedOut = stateAge > 150_000;
      return {
        title: timedOut ? "Speech backend needs attention" : "Starting speech engine",
        message: timedOut
          ? "The local speech backend is taking longer than expected."
          : "The local speech services are starting…",
        hint: timedOut ? "Restart the app; from source, rerun setup if this persists." : "This usually takes a few seconds.",
        action: timedOut ? "retry" : null,
        actionLabel: "Try again",
        autoOpen: timedOut,
        kind: timedOut ? "error" : "busy",
      };
    }
    default:
      return {
        title: "Speech engine unavailable",
        message: "Orpheus could not finish starting its local speech services.",
        hint: "Try again, then restart the app or check the logs if it still fails.",
        action: "retry",
        actionLabel: "Try again",
        autoOpen: true,
        kind: "error",
      };
  }
}

let setupActionKind = "retry";
let lastReadinessNotice = "";

function setSpeechGate(ready, stack, copy = null) {
  const wasReady = engineReady;
  engineReady = !!ready;
  backendModelOperationActive = !!stack?.modelOperationActive;
  const hasPortConflict = !!(stack?.portConflict?.llama || stack?.portConflict?.backend);
  modelManagementReady = !!stack?.configReady && !!stack?.runtimePresent && !!stack?.backendPresent && !hasPortConflict && !stack?.llamaReused;
  const nextState = stack?.state || (ready ? "ready" : "error");
  if (nextState !== currentStackState) readinessStateSince = Date.now();
  currentStackState = nextState;
  el("speak").disabled = !engineReady || switchingLang || installingRuntime || backendModelOperationActive;
  el("speak").title = engineReady ? "" : "Finish setup before speaking";
  updatePickerGate();
  el("lang").title = stack?.llamaReused
    ? "The active voice server is externally managed; restart without it to switch models"
    : "Language";

  const notice = JSON.stringify({
    ready: engineReady,
    state: currentStackState,
    message: copy?.message || "Ready",
    setupRequired: !!copy?.autoOpen,
  });
  let readinessPublish = Promise.resolve();
  if (notice !== lastReadinessNotice) {
    lastReadinessNotice = notice;
    readinessPublish = emitTo("main", "ui:readiness", JSON.parse(notice)).catch(() => {});
  }

  if (engineReady) {
    if (!wasReady) setStatus("idle");
    if (panelState === "setup") setPanelState("idle");
    return readinessPublish;
  }

  const c = copy || setupCopy(stack || { state: currentStackState });
  el("setupTitle").textContent = c.title;
  el("setupMsg").textContent = c.message;
  el("setupHint").textContent = c.hint || "";
  setupActionKind = c.action;
  el("setupAction").textContent = c.actionLabel || "Check again";
  el("setupAction").style.display = c.action ? "block" : "none";
  el("setupAction").disabled = false;
  if (panelState !== "confirm" && panelState !== "downloading") setPanelState("setup");
  setStatus(c.message, c.kind);
  return readinessPublish;
}

async function refreshLiveVoices(preferred) {
  const result = await getVoices();
  // Do not let a late backend response replace a language currently shown in a
  // confirmation/download flow. Retry after the operation settles instead.
  if (pendingAction || probingSelection || switchingLang || installingRuntime) return;
  const revision = selectionRevision;
  const preferredLang = preferred || await currentLangFromBackend();
  if (revision !== selectionRevision || pendingAction || probingSelection || switchingLang) return;
  voicesAreLive = result.ok;
  await setupLangAndVoices(result.voices, preferredLang);
  if (revision !== selectionRevision || pendingAction || probingSelection || switchingLang) return;
  if (!hasStagedModelSelection()) syncVoiceToPet(false);
}

function applyAuthoritativeModelState(state, { preserveSelection = false } = {}) {
  if (!state || typeof state !== "object") return;
  if (Object.prototype.hasOwnProperty.call(state, "loadedQuant") && state.loadedQuant !== undefined) {
    loadedQuant = state.loadedQuant || null;
  }
  if (Object.prototype.hasOwnProperty.call(state, "language")) {
    if (state.language && availableLangs.includes(state.language)) loadedLang = state.language;
    else if (state.language == null && state.modelPresent === false) {
      loadedLang = null;
      loadedQuant = null;
    }
  }
  if (!preserveSelection && state.preferredQuant) {
    currentQuant = state.preferredQuant;
    localStorage.setItem("pet.quant", currentQuant);
    ensureQuantOption(currentQuant);
    el("quality").value = currentQuant;
  }
  if (!preserveSelection && loadedLang) {
    currentLang = loadedLang;
    el("lang").value = currentLang;
    fillVoiceOptions(currentLang);
    localStorage.setItem("pet.lang", currentLang);
  }
}

async function refreshReadiness() {
  const revision = ++readinessRevision;
  let raw;
  try {
    raw = await invoke("stack_status");
  } catch (error) {
    if (revision !== readinessRevision) return;
    console.warn("stack status unavailable:", error);
    const stack = normalizeStackStatus({ state: "error" }, { present: false });
    await setSpeechGate(false, stack, setupCopy(stack));
    return;
  }

  const backendLang = raw?.currentLanguage || await currentLangFromBackend();
  if (revision !== readinessRevision) return;
  const preservePendingSelection = !!pendingAction || probingSelection || switchingLang;
  applyAuthoritativeModelState({
    language: raw?.modelPresent === false ? null : (raw?.currentLanguage ?? null),
    preferredQuant: raw?.preferredQuant,
    loadedQuant: raw?.loadedQuant,
    modelPresent: raw?.modelPresent,
  }, { preserveSelection: preservePendingSelection });
  if (!preservePendingSelection && backendLang && availableLangs.includes(backendLang) && backendLang !== currentLang) {
    currentLang = backendLang;
    el("lang").value = backendLang;
    fillVoiceOptions(backendLang);
  }
  let model = null;
  try { model = await invoke("model_status", { lang: currentLang, quant: currentQuant }); }
  catch { /* expanded stack_status already carries modelPresent */ }
  if (revision !== readinessRevision) return;

  const stack = normalizeStackStatus(raw, model);
  if (
    ["runtime-missing", "backend-missing"].includes(stack.state) &&
    !runtimeInstallPlan &&
    !runtimeInstallPlanLoading
  ) {
    runtimeInstallPlanLoading = true;
    loadRuntimeInstallPlan({ refresh: true }).finally(() => {
      runtimeInstallPlanLoading = false;
    });
  }
  if (stack.backendReady && !voicesAreLive && !preservePendingSelection) {
    try { await refreshLiveVoices(backendLang); }
    catch { /* the built-in voice list remains usable for setup */ }
    if (revision !== readinessRevision) return;
  }
  const operationActive = switchingLang || installingRuntime || stack.modelOperationActive;
  if (stack.ready && !operationActive) {
    await setSpeechGate(true, stack);
  } else if (operationActive) {
    const loadingStack = {
      ...stack,
      ready: false,
      state: installingRuntime ? "runtime-installing" : "loading-model",
    };
    await setSpeechGate(false, loadingStack, setupCopy(loadingStack));
  } else {
    await setSpeechGate(false, stack, setupCopy(stack));
  }
  await rebuildLangOptions({ quant: currentQuant });
}

async function pollReadiness() {
  await refreshReadiness();
  clearTimeout(readinessPoll);
  readinessPoll = window.setTimeout(pollReadiness, engineReady ? 8_000 : 2_500);
}

async function populateVoices() {
  setStatus("checking speech engine…", "busy");
  await refreshLiveVoices(await currentLangFromBackend());
  pollReadiness();
}

// Panel sub-states: idle (text box + Speak) / download-confirm / download-progress.
// The confirm and progress UIs take over the text box + Speak space; the pickers
// stay put above them.
function setPanelState(state) {
  panelState = state;
  const idle = state === "idle";
  el("text").style.display = idle ? "block" : "none";
  el("speakRow").style.display = idle ? "flex" : "none";
  el("setupRow").style.display = state === "setup" ? "flex" : "none";
  el("confirmRow").style.display = state === "confirm" ? "flex" : "none";
  el("dlRow").style.display = state === "downloading" ? "flex" : "none";
}
function setDownloadProgress(pct, label) {
  const f = el("dlFill");
  if (f) f.style.width = `${Math.max(0, Math.min(100, pct))}%`;
  const l = el("dlLabel");
  if (l && label) l.textContent = label;
}

// A confirmation owns a complete, versioned operation. Keeping the action and
// revert together prevents a late availability probe from pairing a new label
// with an older callback.
let pendingAction = null;

function setPendingConfirmation(pending, message, buttonLabel = "Download") {
  pendingAction = pending;
  el("confirmDl").textContent = buttonLabel;
  el("confirmMsg").textContent = message;
  setPanelState("confirm");
  updatePickerGate();
}

async function clearPendingConfirmation({ revert = false } = {}) {
  const pending = pendingAction;
  pendingAction = null;
  selectionRevision += 1;
  probingSelection = false;
  if (revert && pending?.revert) await pending.revert();
  updatePickerGate();
}

async function loadRuntimeInstallPlan({ refresh = false } = {}) {
  try {
    runtimeInstallPlan = await invoke("runtime_install_plan");
  } catch (error) {
    runtimeInstallPlan = { available: false, error: String(error?.message || error) };
  }
  if (refresh && ["runtime-missing", "backend-missing"].includes(currentStackState)) {
    await refreshReadiness();
  }
  return runtimeInstallPlan;
}

async function requestRuntimeInstall() {
  if (!runtimeInstallPlan?.available) await loadRuntimeInstallPlan();
  if (!runtimeInstallPlan?.available) {
    operationError = `Runtime download isn't available: ${String(runtimeInstallPlan?.error || "release asset not found").slice(0, 120)}`;
    operationRetryKind = "runtime";
    await refreshReadiness();
    return;
  }
  const flavor = runtimeInstallPlan.flavor === "cpu" ? "CPU" : "NVIDIA GPU";
  const size = runtimeInstallPlan.approximateBytes
    ? ` (~${(Number(runtimeInstallPlan.approximateBytes) / 1e9).toFixed(1)} GB)`
    : "";
  setPendingConfirmation({
    id: newOperationId("runtime-confirm"),
    kind: "runtime",
    run: doInstallRuntime,
  }, `Download and install the verified ${flavor} speech runtime${size}?`, "Install");
}

async function doInstallRuntime() {
  if (installingRuntime) return;
  installingRuntime = true;
  switchingLang = true;
  operationError = "";
  operationRetryKind = "runtime";
  retryLanguage = null;
  retryQuant = null;
  const installingStack = { state: "runtime-installing" };
  setSpeechGate(false, installingStack, setupCopy(installingStack));
  publishSwitching(true, null);
  el("stopDl").disabled = false;
  setPanelState("downloading");
  setDownloadProgress(0, "Preparing runtime download…");
  try {
    await invoke("install_runtime");
    setDownloadProgress(100, "Runtime installed · speech engine starting…");
  } catch (error) {
    const message = String(error?.message || error);
    if (/cancelled/i.test(message)) {
      setStatus("runtime install cancelled", "warn");
    } else {
      operationError = `Couldn't install the speech runtime: ${message.slice(0, 140)}`;
      operationRetryKind = "runtime";
      setStatus(operationError, "error");
    }
  } finally {
    installingRuntime = false;
    switchingLang = false;
    publishSwitching(false, null);
    setPanelState("idle");
    lastReadinessNotice = "";
    await refreshReadiness();
    setPanelState(engineReady ? "idle" : "setup");
    updatePickerGate();
  }
}

async function restoreLoadedSelection() {
  if (loadedLang && availableLangs.includes(loadedLang)) currentLang = loadedLang;
  if (loadedQuant) currentQuant = loadedQuant;
  ensureQuantOption(currentQuant);
  el("lang").value = currentLang;
  el("quality").value = currentQuant;
  fillVoiceOptions(currentLang);
  stagedVoice = null;
  await rebuildLangOptions({ quant: currentQuant });
}

// Stage the complete language + size tuple. Every edit updates the same existing
// confirmation instead of starting work, even when the candidate is on disk.
async function stageModelSelection({ force = false } = {}) {
  if (switchingLang || installingRuntime || backendModelOperationActive) return;
  if (!modelManagementReady) {
    await restoreLoadedSelection();
    setPanelState("setup");
    setStatus("Install or repair the runtime before changing models", "warn");
    return;
  }

  const targetLang = currentLang;
  const targetQuant = currentQuant;
  const differs = targetLang !== loadedLang || targetQuant !== loadedQuant;
  const revision = ++selectionRevision;
  if (!force && !differs) {
    pendingAction = null;
    probingSelection = false;
    stagedVoice = null;
    fillVoiceOptions(currentLang);
    setPanelState(engineReady ? "idle" : "setup");
    await rebuildLangOptions({ quant: currentQuant });
    updatePickerGate();
    return;
  }

  probingSelection = true;
  const pending = {
    id: newOperationId("selection-confirm"),
    kind: "model-selection",
    revision,
    targetLang,
    targetQuant,
    run: () => doModelSwitch(targetLang, targetQuant),
    revert: restoreLoadedSelection,
  };
  setPendingConfirmation(
    pending,
    `Checking ${LANG_NAMES[targetLang] || targetLang} · ${quantLabel(targetQuant)}…`,
    "Checking…",
  );
  el("confirmDl").disabled = true;

  const rebuild = rebuildLangOptions({ quant: targetQuant });
  let status;
  try {
    status = await invoke("model_status", { lang: targetLang, quant: targetQuant });
  } catch {
    // A failed availability probe must never bypass download consent.
    status = { present: false, sizeBytes: QUANT_BYTES[targetQuant] };
  }
  await rebuild;
  if (
    revision !== selectionRevision ||
    targetLang !== currentLang ||
    targetQuant !== currentQuant
  ) return;

  probingSelection = false;
  const gb = (Number(status.sizeBytes || QUANT_BYTES[targetQuant]) / 1e9).toFixed(1);
  const name = LANG_NAMES[targetLang] || targetLang;
  pendingAction = pending;
  el("confirmDl").textContent = status.present ? "Switch" : "Download & switch";
  el("confirmDl").disabled = false;
  el("confirmMsg").textContent = status.present
    ? `Switch to ${name} · ${quantLabel(targetQuant)}?`
    : `Download and switch to ${name} · ${quantLabel(targetQuant)} (~${gb} GB)?`;
  updatePickerGate();
}

async function onLangChange(target, { force = false } = {}) {
  if (switchingLang || installingRuntime || backendModelOperationActive) {
    el("lang").value = currentLang;
    return;
  }
  currentLang = target;
  fillVoiceOptions(target);
  stagedVoice = el("voice").value || null;
  await stageModelSelection({ force });
}

async function onQuantChange(target, { force = false } = {}) {
  if (switchingLang || installingRuntime || backendModelOperationActive) {
    el("quality").value = currentQuant;
    return;
  }
  currentQuant = target;
  ensureQuantOption(target);
  el("quality").value = target;
  await stageModelSelection({ force });
}

async function doModelSwitch(targetLang, targetQuant) {
  if (switchingLang || installingRuntime || backendModelOperationActive) return false;
  const operationId = newOperationId("model");
  const requestedVoice = stagedVoice || el("voice").value || "";
  operationError = "";
  operationRetryKind = "model";
  retryLanguage = null;
  retryQuant = null;
  switchingLang = true;
  activeOperationId = operationId;
  probingSelection = false;
  const loadingStack = {
    state: "loading-model",
    modelOperationActive: true,
    modelOperation: { active: true, targetLanguage: targetLang, targetQuant },
  };
  setSpeechGate(false, loadingStack, setupCopy(loadingStack));
  publishSwitching(true, operationId);
  setPanelState("downloading");
  el("stopDl").disabled = false;
  const targetName = LANG_NAMES[targetLang] || targetLang;
  setDownloadProgress(0, `Preparing ${targetName} · ${quantLabel(targetQuant)}…`);
  setStatus(`preparing ${targetName}…`, "busy");
  let succeeded = false;
  let selectedVoice = "";
  try {
    const result = await invoke("set_model_selection", {
      lang: targetLang,
      quant: targetQuant,
      operationId,
    });
    if (!result || result.operationId !== operationId) {
      throw new Error("speech engine returned a stale model-switch result");
    }
    // Rust's result is authoritative, including quant fallback.
    applyAuthoritativeModelState(result);
    const resultVoices = voicesForLang(currentLang);
    selectedVoice = resultVoices.includes(requestedVoice)
      ? requestedVoice
      : el("voice").value;
    if (selectedVoice) {
      el("voice").value = selectedVoice;
      localStorage.setItem("pet.voice", selectedVoice);
    }
    stagedVoice = null;
    await rebuildLangOptions({ quant: currentQuant });
    succeeded = true;
  } catch (error) {
    const message = String(error?.message || error);
    if (/cancelled/i.test(message)) {
      setStatus("model switch cancelled", "warn");
    } else {
      console.error(error);
      operationError = `Couldn't prepare ${targetName} · ${quantLabel(targetQuant)}: ${message.slice(0, 120)}`;
      operationRetryKind = "model";
      retryLanguage = targetLang;
      retryQuant = targetQuant;
      setStatus(operationError, "error");
    }
  } finally {
    if (activeOperationId === operationId) activeOperationId = null;
    switchingLang = false;
    publishSwitching(false, operationId);
    setPanelState("idle");
    lastReadinessNotice = "";
    await refreshReadiness();
    stagedVoice = null;
    setPanelState(engineReady ? "idle" : "setup");
    updatePickerGate();
    if (operationError && engineReady) setStatus(operationError, "error");
  }
  if (succeeded && engineReady && selectedVoice) {
    emitTo("main", "ui:voice", { voice: selectedVoice, greet: true }).catch(() => {});
  }
  return succeeded;
}

function requestSpeak() {
  if (!engineReady || hasStagedModelSelection()) {
    if (hasStagedModelSelection()) {
      setStatus("Confirm or cancel the staged model first", "warn");
      return;
    }
    setPanelState("setup");
    setStatus("Finish setup before speaking", "warn");
    return;
  }
  const text = el("text").value.trim();
  if (!text) { el("text").focus(); return; }
  emitTo("main", "ui:speak", { text, voice: el("voice").value }).catch(() => {});
}

function wireUi() {
  el("speak").addEventListener("click", requestSpeak);
  el("hide").addEventListener("click", () => emitTo("main", "ui:hide", {}).catch(() => {}));
  el("voice").addEventListener("change", () => {
    const v = el("voice").value;
    if (hasStagedModelSelection()) {
      stagedVoice = v;
      return;
    }
    localStorage.setItem("pet.voice", v);
    syncVoiceToPet(engineReady); // stay silent until the full stack is ready
  });
  el("lang").addEventListener("change", () => onLangChange(el("lang").value));
  el("quality").addEventListener("change", () => onQuantChange(el("quality").value));
  el("confirmDl").addEventListener("click", async () => {
    const btn = el("confirmDl");
    const pending = pendingAction;
    if (!pending) {
      setPanelState(engineReady ? "idle" : "setup");
      updatePickerGate();
      return;
    }
    btn.disabled = true;
    pendingAction = null;
    el("stopDl").disabled = false;
    setPanelState("downloading");
    setDownloadProgress(0, "Preparing…");
    updatePickerGate();
    try {
      const started = await pending.run();
      if (started === false && !switchingLang && !installingRuntime) {
        setPanelState(engineReady ? "idle" : "setup");
      }
    } catch (error) {
      operationError = `Couldn't start the operation: ${String(error?.message || error).slice(0, 120)}`;
      setStatus(operationError, "error");
      setPanelState(engineReady ? "idle" : "setup");
    } finally {
      btn.disabled = false;
      if (!switchingLang && !installingRuntime && panelState === "downloading") {
        setPanelState(engineReady ? "idle" : "setup");
      }
      updatePickerGate();
    }
  });
  el("confirmCancel").addEventListener("click", async () => {
    await clearPendingConfirmation({ revert: true });
    setPanelState(engineReady ? "idle" : "setup");
    updatePickerGate();
  });
  el("setupAction").addEventListener("click", async () => {
    const btn = el("setupAction");
    if (setupActionKind === "model" || setupActionKind === "quant") {
      currentLang = retryLanguage || loadedLang || currentLang;
      currentQuant = retryQuant || loadedQuant || currentQuant;
      ensureQuantOption(currentQuant);
      el("lang").value = currentLang;
      el("quality").value = currentQuant;
      fillVoiceOptions(currentLang);
      stagedVoice = el("voice").value || null;
      operationError = "";
      retryLanguage = null;
      retryQuant = null;
      await stageModelSelection({ force: true });
      return;
    }
    if (setupActionKind === "runtime") {
      operationError = "";
      await requestRuntimeInstall();
      return;
    }
    btn.disabled = true;
    btn.textContent = "Restarting…";
    operationError = "";
    operationRetryKind = "retry";
    retryLanguage = null;
    retryQuant = null;
    readinessStateSince = Date.now();
    try {
      await invoke("restart_stack");
    } catch (error) {
      operationError = `Couldn't restart the speech stack: ${String(error?.message || error).slice(0, 120)}`;
      operationRetryKind = "retry";
    }
    lastReadinessNotice = "";
    await refreshReadiness();
    if (!engineReady) btn.disabled = false;
  });
  el("stopDl").addEventListener("click", async () => {
    el("stopDl").disabled = true;
    el("dlLabel").textContent = "stopping…";
    try {
      const stopped = installingRuntime
        ? await invoke("cancel_runtime_install")
        : await invoke("cancel_download");
      if (!stopped) el("dlLabel").textContent = "Finishing the current load…";
    } catch { /* the operation's own result will surface the failure */ }
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
      try { current = await invoke("get_hotkey"); }
      catch { /* retain the last confirmed shortcut */ }
      btn.textContent = comboToLabel(current);
      setStatus(`hotkey wasn't changed: ${String(err?.message || err).slice(0, 70)}`, "error");
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
  const revision = selectionRevision;
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
    if (q && revision === selectionRevision && !pendingAction && !switchingLang && !probingSelection) {
      currentQuant = q;
      // A config quant outside our presets (e.g. Q5_K_M) still shows honestly.
      if (!QUANTS.some((x) => x.id === q)) {
        const opt = document.createElement("option");
        opt.value = q;
        opt.textContent = q;
        sel.appendChild(opt);
      }
      sel.value = q;
      await rebuildLangOptions({ quant: q });
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

  const focusPrimaryControl = () => {
    if (panelState === "setup" && setupActionKind) el("setupAction").focus();
    else if (panelState === "idle") el("text").focus();
  };

  // Focus the useful control for the current state when the pet opens us.
  win.onFocusChanged(({ payload }) => {
    if (payload) focusPrimaryControl();
  }).catch(() => {});

  // Status updates from the pet (thinking / speaking / errors).
  await listen("pet:status", (e) => {
    if (!engineReady) return; // setup diagnostics stay visible while speech is gated
    const p = e.payload || {};
    setStatus(p.text, p.kind);
  });
  // The pet just opened us → focus the primary control for this state.
  await listen("pet:panel-open", focusPrimaryControl);
  // The renderer (re)connected → resync and republish readiness. `pet:ready`
  // deliberately does not mean the speech engine itself is ready.
  await listen("pet:ready", () => {
    if (allVoices.length && !hasStagedModelSelection()) syncVoiceToPet(false);
    // The panel can finish first while hidden; force this snapshot to be sent
    // again after the pet confirms its listener is installed.
    lastReadinessNotice = "";
    refreshReadiness();
  });

  // Model download / load progress → the in-panel progress bar (the pet shows
  // the cauldron from the same event).
  await listen("model-progress", (e) => {
    const p = e.payload || {};
    if (!activeOperationId || p.operationId !== activeOperationId) return;
    const name = LANG_NAMES[p.lang] || p.lang || "";
    const gb = (n) => (Number(n || 0) / 1e9).toFixed(1);
    if (p.phase === "download") {
      const loadingStack = {
        state: "loading-model",
        modelOperation: { active: true, targetLanguage: p.lang, targetQuant: p.quant },
      };
      setSpeechGate(false, loadingStack, setupCopy(loadingStack));
      const pct = p.pct ?? 0;
      setPanelState("downloading");
      const detail = p.total ? `${pct}% · ${gb(p.received)}/${gb(p.total)} GB` : `${gb(p.received)} GB`;
      setDownloadProgress(pct, `Downloading ${name} · ${detail}`);
    } else if (p.phase === "loading") {
      const loadingStack = {
        state: "loading-model",
        modelOperation: { active: true, targetLanguage: p.lang, targetQuant: p.quant },
      };
      setSpeechGate(false, loadingStack, setupCopy(loadingStack));
      setPanelState("downloading");
      setDownloadProgress(100, `Loading ${name} model…`);
      // Rust can still cancel a slow load and restore the previous model until
      // it reaches the short commit boundary, so keep that escape hatch live.
      el("stopDl").disabled = false;
    } else if (p.phase === "ready") {
      setDownloadProgress(100, `${name} ${p.quant || ""} ready`.replace(/\s+/g, " ").trim());
      el("stopDl").disabled = true;
    } else if (["failed", "error"].includes(p.phase)) {
      setDownloadProgress(Number(p.pct || 0), p.message || `Couldn't load ${name}`);
      el("stopDl").disabled = true;
    } else if (p.phase === "cancelled") {
      setDownloadProgress(Number(p.pct || 0), `${name} download cancelled`);
      el("stopDl").disabled = true;
    }
  });

  await listen("runtime-progress", (e) => {
    if (!installingRuntime) return;
    const p = e.payload || {};
    const pct = Number(p.pct ?? 0);
    setPanelState("downloading");
    setDownloadProgress(pct, p.message || "Installing speech runtime…");
    if (["activating", "restarting", "ready"].includes(p.phase)) {
      el("stopDl").disabled = true;
    }
  });

  // Deliberately not awaited: the UI stays usable while the engine boots.
  populateVoices();
}

window.addEventListener("DOMContentLoaded", main);
