import {
  getCurrentWindow,
  getAllWindows,
  currentMonitor,
  primaryMonitor,
} from "@tauri-apps/api/window";
import { PhysicalPosition, LogicalPosition } from "@tauri-apps/api/dpi";
import { emitTo, listen } from "@tauri-apps/api/event";
import { createRenderer } from "./pet/renderer.js";
import { LipSync } from "./pet/lipsync.js";
import { synthesize, synthesizeStream } from "./pet/orpheus.js";
import { LINES, VOICE_LANG } from "./pet/samples.js";

// This is the PET window: a fixed-size, never-resized anchor. It renders the
// witch, owns the audio + lip-sync, and drives a SEPARATE "panel" window that
// floats in the free space beside it. Because the pet window never moves or
// resizes when the panel opens, the witch is rock-solid — no jump, no tearing.
const win = getCurrentWindow();
const el = (id) => document.getElementById(id);

let renderer;
let lip;
let speaking = false;
let paused = false;
let switchingLang = false; // engine is mid model-swap (synced from the panel)
let activeModelOperationId = null;
let engineReady = false;   // full backend + model readiness (synced from stack_status)
let setupPromptShown = false;
let currentVoice = "tara"; // synced from the panel; used by autonomous speech
// Monotonic id per speech request: a newer request invalidates the state
// updates (status text, cleanup) of any request it cancelled.
let session = 0;

// Status text lives in the panel window, so route updates there over an event.
function setStatus(text, kind = "") {
  emitTo("panel", "pet:status", { text, kind }).catch(() => {});
}

// Brewing cauldron shown while a language model downloads/loads; the potion
// level rises with the download %. (The cauldron lives on the pet.)
const CAULDRON_TRAVEL = 76; // SVG user units the liquid moves between empty/full
function showCauldron() {
  el("pet").classList.add("loading");
}
function hideCauldron() {
  el("pet").classList.remove("loading");
}
function setCauldronPct(pct, label) {
  const p = Math.max(0, Math.min(100, Number(pct) || 0));
  const liquid = el("liquid");
  if (liquid) {
    const dy = ((100 - p) / 100) * CAULDRON_TRAVEL;
    liquid.setAttribute("transform", `translate(0 ${dy.toFixed(1)})`);
  }
  const t = el("cauldronPct");
  if (t) t.textContent = label || `${Math.round(p)}%`;
}

// A .riv dropped at public/pets/witch.riv is served at /pets/witch.riv.
// Until then, resolvePetSrc returns null and we render the SVG fallback.
// Checking the RIVE magic bytes matters: the dev server answers missing paths
// with index.html (200), which would otherwise look like a real file.
async function resolvePetSrc() {
  try {
    const res = await fetch("/pets/witch.riv", { method: "GET" });
    if (res.ok) {
      const head = new Uint8Array((await res.arrayBuffer()).slice(0, 4));
      if (new TextDecoder().decode(head) === "RIVE") return "/pets/witch.riv";
    }
  } catch { /* not present yet */ }
  return null;
}

function clearPause() {
  if (paused) {
    paused = false;
    renderer.setPaused(false); // rip the tape off
  }
}

// Synthesize + speak `text` in `voice` (defaults to the current voice). Called
// by the panel (Speak/hotkey) and by the pet's own autonomous lines.
async function speak(text, voice) {
  if (switchingLang || !engineReady) return; // never probe a partial stack with synthesis
  text = (typeof text === "string" ? text : "").trim();
  if (!text) return;
  voice = voice || currentVoice || "tara";

  // New input resets whatever is happening — playing or paused: cancel the
  // old session and start fresh.
  if (speaking) {
    lip.stop(); // aborts the stream + sources, unfreezes a suspended context
    clearPause();
  }
  const my = ++session;

  speaking = true;
  setStatus("thinking…", "busy");
  renderer.setTalking(false);
  // Thought-bubble pose from now until the first audio is actually ready.
  renderer.setThinking(true);

  try {
    // Streaming first: audio starts after a few decoded frames instead of
    // after the whole clip is generated. Falls back to the buffered endpoint
    // if the stream route isn't available (older server build).
    let streamed = false;
    try {
      const res = await synthesizeStream(text, voice);
      if (session !== my) return;
      await lip.playStream(res, {
        onStart: () => {
          if (session !== my) return;
          renderer.setThinking(false);
          if (paused) {
            // User paused during the thinking phase — stay taped.
            setStatus("paused — click me to resume", "warn");
            return;
          }
          setStatus("speaking", "busy");
          renderer.setTalking(true);
        },
      });
      streamed = true;
    } catch (streamErr) {
      if (session !== my) return;
      const msg = String(streamErr?.message || streamErr);
      if (msg.includes("empty audio")) throw streamErr; // backend down — don't retry
      console.warn("streaming unavailable, falling back to buffered synth:", streamErr);
    }
    if (session !== my) return;
    if (!streamed) {
      const wav = await synthesize(text, voice);
      if (session !== my) return;
      // A bare-header WAV (~44 bytes) means the FastAPI answered 200 but its
      // inference backend was unreachable — surface that instead of silence.
      if (wav.byteLength < 1000) {
        throw new Error("empty audio from engine (inference backend unreachable?)");
      }
      renderer.setThinking(false);
      setStatus("speaking", "busy");
      renderer.setTalking(true);
      await lip.play(wav);
      if (session !== my) return;
    }
    setStatus("idle");
  } catch (err) {
    if (session !== my) return;
    console.error(err);
    const msg = String(err?.message || err);
    setStatus(
      msg.includes("empty audio") ? "backend unreachable — see logs" : "engine not ready?",
      "error"
    );
  } finally {
    // Only the still-current session cleans up — a cancelled one must not
    // clobber the state of the session that replaced it.
    if (session === my) {
      renderer.setThinking(false);
      renderer.setTalking(false);
      renderer.setMouthOpen(0);
      clearPause();
      speaking = false;
    }
  }
}

// Pick a random entry (never the same one twice running) from a set and speak
// it. Shared by idle-click samples, voice-change greetings, and show/hide lines.
const _lastPick = {};
function speakRandom(key, list, transform) {
  if (!list || !list.length) return;
  let i = 0;
  if (list.length > 1) {
    do { i = Math.floor(Math.random() * list.length); } while (i === _lastPick[key]);
  }
  _lastPick[key] = i;
  speak(transform ? transform(list[i]) : list[i]);
}

// Every spoken snippet is localized to the selected voice's language (a Korean
// voice speaks Korean lines, etc.), falling back to English.
function langForVoice(voice) {
  return VOICE_LANG[voice] || "en";
}
function lines(lang, category) {
  const set = LINES[lang] && LINES[lang][category];
  return set && set.length ? set : LINES.en[category];
}

// Left-click while idle: say a random sample line in the current voice's
// language.
function speakSample() {
  const lang = langForVoice(currentVoice);
  speakRandom(`sample:${lang}`, lines(lang, "samples"));
}
// Voice change: introduce herself in the newly selected voice + language.
function greetInVoice(name) {
  const lang = langForVoice(name);
  speakRandom(`greet:${lang}`, lines(lang, "greetings"), (t) => t.replaceAll("{name}", name));
}
// Show / hide: a quick hello when she appears, a quick goodbye when she tucks
// away — in the current voice's language.
function sayHello() {
  const lang = langForVoice(currentVoice);
  speakRandom(`hello:${lang}`, lines(lang, "hellos"));
}
function sayGoodbye() {
  const lang = langForVoice(currentVoice);
  speakRandom(`bye:${lang}`, lines(lang, "goodbyes"));
}

// Click-while-talking: freeze the audio mid-word (duct tape on), click again
// to rip it off and continue exactly where she left off.
async function togglePause() {
  if (!speaking) return;
  if (!paused) {
    if (!(await lip.pause())) return;
    paused = true;
    renderer.setTalking(false);
    renderer.setThinking(false);
    renderer.setMouthOpen(0);
    renderer.setPaused(true); // slap the tape on
    setStatus("paused — click me to resume", "warn");
  } else {
    paused = false;
    renderer.setPaused(false); // rip it off
    await lip.resume();
    renderer.setTalking(true);
    setStatus("speaking", "busy");
  }
}

// ---- Follower popup placement --------------------------------------------
// The pet window is the fixed anchor. The panel is its OWN window, positioned
// in the free space beside the pet and clamped fully on-screen. Opening the
// panel never touches the pet window, so the witch never moves.
const PET_W = 148, PET_H = 280, PANEL_W = 320, PANEL_H = 260;
const GAP = 8;   // space between the pet and the panel
const PAD = 16;  // transparent margin inside the panel window for its shadow
const TASKBAR = 48; // logical px reserved at the bottom of the work area

let panelWin = null;   // the separate "panel" window handle
let panelOpen = false;

function clampNum(v, lo, hi) { return Math.max(lo, Math.min(hi, v)); }

async function monScale() {
  const m = (await currentMonitor()) || (await primaryMonitor());
  return m ? m.scaleFactor || 1 : 1;
}
// The pet window's rect in logical px (outerPosition is physical).
async function petScreenRect() {
  const s = await monScale();
  const p = await win.outerPosition();
  return { x: p.x / s, y: p.y / s, w: PET_W, h: PET_H };
}
// Work area in logical px (reserving the taskbar at the bottom).
async function workArea() {
  const m = (await currentMonitor()) || (await primaryMonitor());
  if (!m) return null;
  const s = m.scaleFactor || 1;
  return {
    left: m.position.x / s,
    top: m.position.y / s,
    right: (m.position.x + m.size.width) / s,
    bottom: (m.position.y + m.size.height) / s - TASKBAR,
  };
}
// Pick which side of the pet the popup sits on. Horizontal sides first, so
// corners open to the side; otherwise open away from the nearest edge.
function chooseSide(P, area) {
  const above = P.y - area.top;
  const below = area.bottom - (P.y + PET_H);
  const left = P.x - area.left;
  const right = area.right - (P.x + PET_W);
  const cands = [
    { side: "right", near: left, room: right, need: GAP + PANEL_W },
    { side: "left", near: right, room: left, need: GAP + PANEL_W },
    { side: "below", near: above, room: below, need: GAP + PANEL_H },
    { side: "above", near: below, room: above, need: GAP + PANEL_H },
  ];
  cands.sort((a, b) => a.near - b.near);
  for (const c of cands) if (c.room >= c.need) return c.side;
  return cands.reduce((a, b) => (b.room - b.need > a.room - a.need ? b : a)).side;
}
// The popup's CONTENT rect adjacent to the pet on `side`, slid so the whole
// window (content + PAD shadow room) stays on-screen.
function panelContentRectFor(P, side, area) {
  let x, y;
  if (side === "right") { x = P.x + PET_W + GAP; y = P.y; }
  else if (side === "left") { x = P.x - GAP - PANEL_W; y = P.y; }
  else if (side === "below") { x = P.x; y = P.y + PET_H + GAP; }
  else { x = P.x; y = P.y - GAP - PANEL_H; }
  x = clampNum(x, area.left + PAD, area.right - PANEL_W - PAD);
  y = clampNum(y, area.top + PAD, area.bottom - PANEL_H - PAD);
  return { x, y };
}

// Move the panel window to the free side of the pet. The window's top-left is
// the content rect minus the PAD shadow margin.
async function placePanel() {
  if (!panelWin) return;
  const area = await workArea();
  const P = await petScreenRect();
  const c = area
    ? panelContentRectFor(P, chooseSide(P, area), area)
    : { x: P.x, y: P.y - GAP - PANEL_H };
  await panelWin.setPosition(new LogicalPosition(Math.round(c.x - PAD), Math.round(c.y - PAD)));
}

// Keep at most one placement in flight: each pet move that arrives while the
// previous placement is still resolving is dropped, and the next move catches
// up. This tracks the drag live without depending on requestAnimationFrame,
// which Windows can starve during the OS move loop.
let following = false;
function scheduleFollow() {
  if (!panelOpen || following) return;
  following = true;
  placePanel().catch(() => {}).finally(() => { following = false; });
}

async function openPanel() {
  if (!panelWin) return;
  await placePanel();      // position BEFORE showing → appears in the right spot
  await panelWin.show();
  await panelWin.setFocus();
  panelOpen = true;
  emitTo("panel", "pet:panel-open", {}).catch(() => {});
}
async function closePanel() {
  if (!panelWin || !panelOpen) return;
  panelOpen = false;
  await panelWin.hide();
}
async function togglePanel(force) {
  const want = force ?? !panelOpen;
  if (want) await openPanel();
  else await closePanel();
}

// ---- Drag + click ---------------------------------------------------------
// Drag the pet window by dragging the witch; a click without movement talks or
// pauses; right-click toggles the panel. startDragging() hands the gesture to
// the OS after a small threshold so clicks and drags stay distinct.
function wireDragAndClick() {
  const pet = el("pet");
  let start = null;
  let moved = false;

  pet.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    start = { x: e.clientX, y: e.clientY };
    moved = false;
  });

  window.addEventListener("mousemove", (e) => {
    if (!start) return;
    if (!moved && Math.abs(e.clientX - start.x) + Math.abs(e.clientY - start.y) > 4) {
      moved = true;
      win.startDragging();
    }
  });

  window.addEventListener("mouseup", () => {
    // A left-click without a drag: pause/resume while she's speaking, say a
    // sample when ready, or expose first-run setup when speech is unavailable.
    if (start && !moved) {
      if (speaking) togglePause();
      else if (engineReady) speakSample();
      else togglePanel(true);
    }
    start = null;
  });

  // Right-click the pet toggles the follower panel.
  pet.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    togglePanel();
  });

  // Feel native: suppress the browser context menu everywhere on the pet.
  window.addEventListener("contextmenu", (e) => e.preventDefault());
}

// ---- Bounce back into view when dragged off the screen ------------------
let bouncing = false;
let settleTimer = 0;
const TASKBAR_MARGIN = 48; // logical px kept clear at the bottom (taskbar)

// Overshoot easing → the window springs past the edge then settles (a bounce).
function easeOutBack(t) {
  const c1 = 1.70158;
  const c3 = c1 + 1;
  return 1 + c3 * Math.pow(t - 1, 3) + c1 * Math.pow(t - 1, 2);
}

async function animateBounce(fromX, fromY, toX, toY) {
  bouncing = true;
  const dur = 420;
  const t0 = performance.now();
  await new Promise((resolve) => {
    const step = (now) => {
      const t = Math.min(1, (now - t0) / dur);
      const e = easeOutBack(t);
      const x = Math.round(fromX + (toX - fromX) * e);
      const y = Math.round(fromY + (toY - fromY) * e);
      win.setPosition(new PhysicalPosition(x, y));
      if (panelOpen) scheduleFollow(); // keep the popup glued through the bounce
      if (t < 1) requestAnimationFrame(step);
      else resolve();
    };
    requestAnimationFrame(step);
  });
  await win.setPosition(new PhysicalPosition(toX, toY)); // land exactly on target
  bouncing = false;
}

// If the pet window sits past the current monitor's work area, spring it back.
async function bounceIntoViewIfNeeded() {
  if (bouncing) return;
  const mon = (await currentMonitor()) || (await primaryMonitor());
  if (!mon) return;
  const [pos, size] = [await win.outerPosition(), await win.outerSize()];
  const margin = Math.round(TASKBAR_MARGIN * (mon.scaleFactor || 1));
  const minX = mon.position.x;
  const minY = mon.position.y;
  const maxX = mon.position.x + mon.size.width - size.width;
  const maxY = mon.position.y + mon.size.height - size.height - margin;
  const tx = Math.max(minX, Math.min(maxX, pos.x));
  const ty = Math.max(minY, Math.min(maxY, pos.y));
  if (tx === pos.x && ty === pos.y) return; // fully in view — nothing to do
  await animateBounce(pos.x, pos.y, tx, ty);
}

// After a drag settles: pull the pet fully on-screen, then re-place the popup
// on whatever side is now free.
async function onDragSettled() {
  if (bouncing) return;
  await bounceIntoViewIfNeeded();
  if (panelOpen) await placePanel();
}

async function setupBounce() {
  try {
    // The popup follows live while the pet moves; when the pet stops (drag
    // released), re-place the popup and bounce back on-screen if needed.
    await win.onMoved(() => {
      if (panelOpen) scheduleFollow();
      if (bouncing) return;
      clearTimeout(settleTimer);
      settleTimer = setTimeout(onDragSettled, 140);
    });
  } catch (e) {
    console.warn("drag handling disabled:", e);
  }
}

async function main() {
  const stage = el("petStage");
  const src = await resolvePetSrc();
  renderer = await createRenderer(stage, { src });
  lip = new LipSync({ onLevel: (v) => renderer.setMouthOpen(v) });

  try {
    const wins = await getAllWindows();
    panelWin = wins.find((w) => w.label === "panel") || null;
  } catch (e) {
    console.warn("panel window unavailable:", e);
  }

  wireDragAndClick();
  setupBounce();

  // ---- Commands from the panel window ----
  await listen("ui:speak", (e) => {
    if (!engineReady) {
      togglePanel(true);
      return;
    }
    const p = e.payload || {};
    speak(p.text, p.voice);
  });
  await listen("ui:voice", (e) => {
    const p = e.payload || {};
    if (!p.voice) return;
    currentVoice = p.voice;
    if (p.greet && engineReady) greetInVoice(p.voice);
  });
  await listen("ui:readiness", (e) => {
    const p = e.payload || {};
    engineReady = !!p.ready;
    if (engineReady) {
      setupPromptShown = false;
      return;
    }
    // Actionable first-run/repair states open once automatically. Ordinary
    // backend/model startup stays quiet unless it exceeds the panel's grace time.
    if (p.setupRequired && !setupPromptShown) {
      setupPromptShown = true;
      togglePanel(true);
    }
  });
  await listen("ui:switching", (e) => {
    const p = e.payload || {};
    const operationId = p.operationId ? String(p.operationId) : null;
    if (!p.active && operationId && activeModelOperationId && operationId !== activeModelOperationId) {
      return; // a late completion cannot hide a newer operation's cauldron
    }
    if (p.active && operationId) activeModelOperationId = operationId;
    if (!p.active) activeModelOperationId = null;
    switchingLang = !!p.active;
    if (!p.active) hideCauldron(); // swap finished/failed — put the witch back
  });
  await listen("ui:hide", async () => {
    sayGoodbye();
    await closePanel();
    await win.hide();
  });
  await listen("ui:close", () => {
    closePanel();
  });

  // ---- Events from the Rust side ----
  // Global hotkey (default Ctrl+Alt+A — see stack.config.json): the Rust side
  // grabs the text highlighted in whatever app has focus and sends it here.
  await listen("speak-selection", async (e) => {
    let text = (e.payload || "").trim();
    if (!text) {
      setStatus("no text selected", "warn");
      return;
    }
    if (!engineReady) {
      // A shortcut must never reveal a pet the user tucked into the tray. If
      // she is already visible, keep the existing setup-recovery affordance.
      let visible = false;
      try { visible = await win.isVisible(); }
      catch { /* safest fallback is to preserve hidden state */ }
      if (visible) await togglePanel(true);
      return;
    }
    if (text.length > 1500) {
      text = text.slice(0, 1500);
      setStatus("long selection — trimmed", "warn");
    }
    speak(text, currentVoice); // new input resets any current (or paused) speech
  });

  // Show/hide (tray menu, tray-icon click, or close-to-tray): greet or farewell.
  // Hiding also tucks the follower panel away.
  await listen("pet-visibility", async (e) => {
    if (e.payload === "show") {
      if (engineReady) sayHello();
    } else if (e.payload === "hide") {
      await closePanel();
      sayGoodbye();
    }
  });

  // Model download / load progress while switching languages → cauldron fill.
  await listen("model-progress", (e) => {
    const p = e.payload || {};
    const operationId = p.operationId ? String(p.operationId) : null;
    const terminal = ["ready", "failed", "cancelled"].includes(p.phase);
    if (terminal && operationId && !activeModelOperationId) {
      return; // no matching model operation is visible in this window
    }
    if (operationId && activeModelOperationId && operationId !== activeModelOperationId) {
      return; // queued delivery from an older switch
    }
    if (operationId && !terminal) activeModelOperationId = operationId;
    if (p.phase === "preparing") {
      showCauldron();
      setCauldronPct(0, "preparing");
    } else if (p.phase === "download") {
      showCauldron();
      setCauldronPct(p.pct ?? 0);
    } else if (p.phase === "loading") {
      showCauldron();
      setCauldronPct(100, "brewing");
    } else if (terminal) {
      activeModelOperationId = null;
      switchingLang = false;
      hideCauldron();
    }
  });

  // Runtime installation uses the same cauldron, but only after the panel has
  // obtained explicit download consent and marked the engine as switching.
  await listen("runtime-progress", (e) => {
    if (!switchingLang) return;
    const p = e.payload || {};
    showCauldron();
    const label = p.phase?.startsWith("verifying")
      ? "checking"
      : p.phase === "extracting" || p.phase === "installing"
        ? "unpacking"
        : p.phase === "activating" || p.phase === "restarting"
          ? "starting"
          : undefined;
    setCauldronPct(p.pct ?? 0, label);
  });

  // Tell the panel we're ready so it can (re)sync the current voice to us.
  emitTo("panel", "pet:ready", {}).catch(() => {});
}

window.addEventListener("DOMContentLoaded", main);
