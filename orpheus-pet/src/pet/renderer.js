// Pet renderer abstraction.
//
// Two implementations share one contract so the rest of the app never cares
// which is active:
//     setMouthOpen(v)  v in 0..1   -> how wide the mouth is (lip-sync)
//     setTalking(bool)             -> idle vs talking body motion
//     setThinking(bool)            -> "model is working, no audio yet" pose
//     setPaused(bool)              -> playback paused (duct tape over mouth)
//     destroy()
//
// FallbackRenderer draws a hand-made SVG witch and works with zero art assets,
// so the whole pipeline is runnable today. RiveRenderer loads a .riv authored
// from the Tripo hero design once it exists. See RIVE-RIG-SPEC for the exact
// state-machine contract the artist must match.
import { Rive, Layout, Fit, Alignment, RuntimeLoader } from "@rive-app/canvas";

// Serve the Rive WASM from our own bundle (public/rive.wasm) instead of a CDN,
// so the pet works offline and under a strict CSP. Only fetched lazily when a
// RiveRenderer is actually constructed.
RuntimeLoader.setWasmUrl("/rive.wasm");

// ---- Rive rig contract (the artist must match these names) ----
export const STATE_MACHINE = "pet";
export const INPUT_MOUTH = "mouthOpen"; // Number input, range 0..100 (percent)
export const INPUT_TALKING = "talking"; // Boolean input
export const INPUT_THINKING = "thinking"; // Boolean input (optional in the rig)
export const INPUT_PAUSED = "paused"; // Boolean input (optional in the rig)

// ---------------------------------------------------------------------------
// Fallback: a cute SVG witch, no external assets required.
// ---------------------------------------------------------------------------
class FallbackRenderer {
  constructor(stage) {
    this.stage = stage;
    this.mouth = null;
    this.eyes = null;
    this.blinkTimer = 0;
  }

  async init() {
    this.stage.innerHTML = `
<svg id="witch" viewBox="38 4 124 234" xmlns="http://www.w3.org/2000/svg">
  <ellipse class="shadow" cx="100" cy="228" rx="44" ry="7"/>
  <g id="witchBody">
    <path class="cloak" d="M58 152 Q100 124 142 152 L158 214 Q100 202 42 214 Z"/>
    <circle class="face" cx="100" cy="120" r="40"/>
    <circle class="cheek" cx="74" cy="130" r="7"/>
    <circle class="cheek" cx="126" cy="130" r="7"/>
    <g id="eyes">
      <circle class="eye" cx="86" cy="114" r="6"/>
      <circle class="eye" cx="114" cy="114" r="6"/>
    </g>
    <ellipse id="mouth" cx="100" cy="137" rx="11" ry="3"/>
    <g id="hat">
      <path class="cone" d="M100 12 L130 82 Q100 72 70 82 Z"/>
      <path class="band" d="M73 80 Q100 70 127 80 L125 71 Q100 62 75 71 Z"/>
      <ellipse class="brim" cx="100" cy="84" rx="54" ry="12"/>
      <circle class="star" cx="100" cy="46" r="3"/>
    </g>
    <!-- Duct tape over the mouth while paused (slap on / rip off via CSS) -->
    <g transform="rotate(-8 100 138)">
      <g id="tape">
        <rect class="tape-base" x="76" y="128" width="48" height="19" rx="2"/>
        <line class="tape-line" x1="81" y1="133.5" x2="119" y2="133.5"/>
        <line class="tape-line" x1="81" y1="141.5" x2="117" y2="141.5"/>
        <path class="tape-edge" d="M76 130 l-4 3 4 4 z"/>
        <path class="tape-edge" d="M124 145 l4 -3 -4 -4 z"/>
      </g>
    </g>
  </g>
  <!-- Thought bubble: shown via the .thinking class while the model works -->
  <g id="thoughtBubble">
    <circle class="tb-trail" cx="73" cy="66" r="3.5"/>
    <circle class="tb-trail" cx="71" cy="54" r="5"/>
    <ellipse class="tb-cloud" cx="64" cy="34" rx="21" ry="15"/>
    <circle class="tb-dot d1" cx="55" cy="35" r="3"/>
    <circle class="tb-dot d2" cx="64" cy="35" r="3"/>
    <circle class="tb-dot d3" cx="73" cy="35" r="3"/>
  </g>
</svg>`;
    this.mouth = this.stage.querySelector("#mouth");
    this.eyes = this.stage.querySelector("#eyes");
    this._scheduleBlink();
  }

  setMouthOpen(v) {
    if (!this.mouth) return;
    const clamped = Math.max(0, Math.min(1, v));
    const ry = 3 + clamped * 17; // 3px closed -> 20px wide open
    const rx = 11 + clamped * 3;
    this.mouth.setAttribute("ry", ry.toFixed(1));
    this.mouth.setAttribute("rx", rx.toFixed(1));
  }

  setTalking(on) {
    this.stage.classList.toggle("talking", !!on);
  }

  setThinking(on) {
    this.stage.classList.toggle("thinking", !!on);
  }

  setPaused(on) {
    const tape = this.stage.querySelector("#tape");
    if (!tape) return;
    if (on) {
      tape.classList.remove("rip");
      tape.classList.remove("slap");
      // Force a reflow so re-adding the class restarts the slap animation.
      void tape.getBoundingClientRect();
      tape.classList.add("slap");
    } else if (tape.classList.contains("slap")) {
      tape.classList.remove("slap");
      tape.classList.add("rip");
    }
  }

  _scheduleBlink() {
    const blink = () => {
      if (!this.eyes) return;
      this.eyes.classList.add("blink");
      setTimeout(() => this.eyes && this.eyes.classList.remove("blink"), 130);
    };
    this.blinkTimer = setInterval(blink, 4200);
  }

  destroy() {
    clearInterval(this.blinkTimer);
    this.stage.innerHTML = "";
  }
}

// ---------------------------------------------------------------------------
// Rive: loads a .riv authored to the contract above.
// ---------------------------------------------------------------------------
class RiveRenderer {
  constructor(stage, src) {
    this.stage = stage;
    this.src = src;
    this.rive = null;
    this.inputs = {};
  }

  async init() {
    const canvas = document.createElement("canvas");
    canvas.width = 440;
    canvas.height = 560;
    canvas.className = "rive-canvas";
    this.stage.innerHTML = "";
    this.stage.appendChild(canvas);
    this.canvas = canvas;

    await new Promise((resolve, reject) => {
      this.rive = new Rive({
        src: this.src,
        canvas,
        autoplay: true,
        stateMachines: STATE_MACHINE,
        layout: new Layout({ fit: Fit.Contain, alignment: Alignment.Center }),
        onLoad: () => {
          this.rive.resizeDrawingSurfaceToCanvas();
          const inputs = this.rive.stateMachineInputs(STATE_MACHINE) || [];
          for (const inp of inputs) this.inputs[inp.name] = inp;
          resolve();
        },
        onLoadError: (e) => reject(e),
      });
    });
  }

  setMouthOpen(v) {
    const inp = this.inputs[INPUT_MOUTH];
    if (inp) inp.value = Math.max(0, Math.min(100, v * 100));
  }

  setTalking(on) {
    const inp = this.inputs[INPUT_TALKING];
    if (inp) inp.value = !!on;
  }

  setThinking(on) {
    // Optional in the rig — silently ignored if the artist didn't add it.
    const inp = this.inputs[INPUT_THINKING];
    if (inp) inp.value = !!on;
  }

  setPaused(on) {
    // Optional in the rig — silently ignored if the artist didn't add it.
    const inp = this.inputs[INPUT_PAUSED];
    if (inp) inp.value = !!on;
  }

  destroy() {
    if (this.rive) {
      try { this.rive.cleanup(); } catch { /* noop */ }
      this.rive = null;
    }
    this.stage.innerHTML = "";
  }
}

// Build a renderer: try Rive if a src is given, otherwise the SVG fallback.
export async function createRenderer(stage, { src } = {}) {
  if (src) {
    try {
      const r = new RiveRenderer(stage, src);
      await r.init();
      return r;
    } catch (err) {
      console.warn("Rive load failed; using fallback witch:", err);
    }
  }
  const fb = new FallbackRenderer(stage);
  await fb.init();
  return fb;
}
