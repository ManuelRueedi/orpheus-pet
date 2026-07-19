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
// Fallback: a hand-inked hedge mage, no external assets required.
// ---------------------------------------------------------------------------
class FallbackRenderer {
  constructor(stage) {
    this.stage = stage;
    this.mouthRig = null;
    this.closedSmile = null;
    this.tongue = null;
    this.teeth = null;
    this.eyes = null;
    this.mouthTarget = 0;
    this.mouthValue = 0;
    this.mouthRaf = 0;
    this.mouthTickAt = 0;
    this.blinkTimer = 0;
    this.blinkResetTimer = 0;
    this.doubleBlinkTimer = 0;
    this.glanceTimer = 0;
    this.glanceResetTimer = 0;
    this.gestureTimer = 0;
    this.gestureResetTimer = 0;
    this.talking = false;
    this.thinking = false;
    this.paused = false;
    this.reducedMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
  }

  async init() {
    this.stage.innerHTML = `
<svg id="witch" viewBox="30 0 140 246" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="A friendly gender-neutral silver-haired hedge mage">
  <defs>
    <linearGradient id="tunicGradient" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#446348"/>
      <stop offset="0.48" stop-color="#2f4b39"/>
      <stop offset="1" stop-color="#172d29"/>
    </linearGradient>
    <linearGradient id="capeGradient" x1="0.1" y1="0" x2="0.9" y2="1">
      <stop offset="0" stop-color="#27384d"/>
      <stop offset="0.52" stop-color="#17283c"/>
      <stop offset="1" stop-color="#0d1726"/>
    </linearGradient>
    <linearGradient id="hatGradient" x1="0.15" y1="0" x2="0.9" y2="1">
      <stop offset="0" stop-color="#344a66"/>
      <stop offset="0.48" stop-color="#21344e"/>
      <stop offset="1" stop-color="#0c1829"/>
    </linearGradient>
    <linearGradient id="brimGradient" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#2d4260"/>
      <stop offset="0.55" stop-color="#172a43"/>
      <stop offset="1" stop-color="#091522"/>
    </linearGradient>
    <linearGradient id="hairGradient" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#f0eee7"/>
      <stop offset="0.42" stop-color="#c9cbd0"/>
      <stop offset="0.72" stop-color="#9099a6"/>
      <stop offset="1" stop-color="#5c6878"/>
    </linearGradient>
    <linearGradient id="hairShadeGradient" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#aeb4bd"/>
      <stop offset="1" stop-color="#596574"/>
    </linearGradient>
    <linearGradient id="leatherGradient" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#8a603a"/>
      <stop offset="0.5" stop-color="#5c3c27"/>
      <stop offset="1" stop-color="#2f211a"/>
    </linearGradient>
    <linearGradient id="armorGradient" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#50443b"/>
      <stop offset="1" stop-color="#28231f"/>
    </linearGradient>
    <linearGradient id="bootGradient" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#765035"/>
      <stop offset="1" stop-color="#38251b"/>
    </linearGradient>
    <linearGradient id="metalGradient" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#e2c27c"/>
      <stop offset="0.5" stop-color="#a87534"/>
      <stop offset="1" stop-color="#5a391e"/>
    </linearGradient>
    <radialGradient id="faceGradient" cx="42%" cy="28%" r="78%">
      <stop offset="0" stop-color="#ffe7bd"/>
      <stop offset="0.68" stop-color="#e7bd83"/>
      <stop offset="1" stop-color="#b98252"/>
    </radialGradient>
    <radialGradient id="eyeGradient" cx="36%" cy="24%" r="78%">
      <stop offset="0" stop-color="#4f5560"/>
      <stop offset="0.3" stop-color="#171b22"/>
      <stop offset="1" stop-color="#020305"/>
    </radialGradient>
    <linearGradient id="greenPotion" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#8fd6a6"/>
      <stop offset="1" stop-color="#1f704e"/>
    </linearGradient>
    <linearGradient id="bluePotion" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#8dd5e8"/>
      <stop offset="1" stop-color="#2b6f91"/>
    </linearGradient>
    <filter id="starGlow" x="-150%" y="-150%" width="400%" height="400%">
      <feGaussianBlur stdDeviation="1.15" result="blur"/>
      <feMerge><feMergeNode in="blur"/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
    <filter id="softDepth" x="-20%" y="-20%" width="140%" height="150%">
      <feDropShadow dx="0" dy="1.1" stdDeviation="0.9" flood-color="#05080d" flood-opacity="0.58"/>
    </filter>
  </defs>

  <ellipse class="shadow" cx="100" cy="237" rx="44" ry="6"/>
  <g id="witchFloat">
    <g id="witchBody">
      <g id="cloakGroup">
        <path class="back-cape" d="M73 151 Q100 143 127 152 Q143 165 149 191 L157 218 Q147 226 137 214 Q123 225 108 214 Q99 227 89 214 Q74 225 61 214 Q50 224 43 216 L50 190 Q57 165 73 151 Z"/>
        <path class="cape-shadow" d="M111 153 Q134 159 143 184 L151 216 Q143 220 136 209 Q124 219 112 211 Z"/>
        <path class="cape-fold" d="M67 162 Q62 187 61 210 M90 155 Q87 184 89 212 M113 154 Q117 184 112 212 M134 165 Q140 190 137 210"/>
        <path class="cape-edge" d="M47 216 Q55 221 62 212 Q75 222 89 212 Q100 223 109 212 Q123 222 137 211 Q146 222 154 216"/>
      </g>

      <path class="leg left-leg" d="M75 197 Q84 193 94 198 L93 222 Q84 228 74 223 Z"/>
      <path class="leg right-leg" d="M106 198 Q116 193 125 198 L130 222 Q120 228 109 223 Z"/>
      <path class="boot-cuff left-cuff" d="M71 214 Q83 209 94 216 L93 224 Q82 228 71 223 Z"/>
      <path class="boot-cuff right-cuff" d="M107 216 Q119 210 130 216 L133 224 Q120 228 108 224 Z"/>
      <path class="boot left-boot" d="M70 220 Q82 216 93 222 L91 234 Q76 239 61 233 L61 227 Z"/>
      <path class="boot right-boot" d="M108 222 Q121 216 132 221 L141 230 L138 235 Q123 239 109 233 Z"/>
      <path class="boot-panel" d="M73 221 Q82 225 91 221 M111 224 Q121 228 131 222"/>
      <path class="boot-sole" d="M61 233 Q76 239 91 234 M109 233 Q123 239 138 235"/>

      <path class="tunic" d="M75 154 Q100 147 125 155 L134 205 Q118 215 101 210 Q83 216 66 206 Z"/>
      <path class="tunic-panel" d="M93 157 L108 155 L112 209 Q101 214 89 210 Z"/>
      <path class="tunic-stitch" d="M98 160 L101 208 M72 201 Q99 210 129 201"/>
      <path class="tunic-hem" d="M68 204 Q83 211 100 207 Q118 212 132 203"/>

      <g id="leftArm">
        <path class="sleeve" d="M73 158 Q58 160 52 178 L49 190 Q55 196 64 191 L78 171 Z"/>
        <path class="shoulder-armor" d="M71 156 Q58 155 51 166 L55 176 Q66 177 76 169 Z"/>
        <path class="armor-seam" d="M56 166 Q64 168 72 163"/>
        <path class="sleeve-cuff" d="M50 180 Q58 185 67 181 L68 191 Q59 198 49 191 Z"/>
        <path class="bracer-panel" d="M52 182 L64 184 L63 191 Q57 194 51 190 Z"/>
        <path class="hand" d="M49 187 Q42 190 45 197 Q48 201 52 196 Q56 202 60 197 Q61 191 56 188 Z"/>
        <path class="glove" d="M45 187 Q52 183 58 189 L59 194 Q52 191 45 196 Z"/>
        <path class="finger-line" d="M48 191 L53 194 M53 189 L57 192"/>
      </g>
      <g id="rightArm">
        <path class="sleeve" d="M127 158 Q142 161 148 178 L151 190 Q145 196 136 191 L122 171 Z"/>
        <path class="shoulder-armor" d="M129 156 Q142 155 149 166 L145 176 Q134 177 124 169 Z"/>
        <path class="armor-seam" d="M144 166 Q136 168 128 163"/>
        <path class="sleeve-cuff" d="M133 181 Q142 185 150 180 L151 191 Q141 198 132 191 Z"/>
        <path class="bracer-panel" d="M136 184 L148 182 L149 190 Q143 194 137 191 Z"/>
        <path class="hand" d="M151 187 Q158 190 155 197 Q152 201 148 196 Q144 202 140 197 Q139 191 144 188 Z"/>
        <path class="glove" d="M142 189 Q148 183 155 187 L155 196 Q148 191 141 194 Z"/>
        <path class="finger-line" d="M152 191 L147 194 M147 189 L143 192"/>
      </g>

      <g id="headGroup">
        <path class="hair-back" d="M65 114 Q67 96 82 88 Q99 80 118 88 Q134 97 136 116 L140 139 Q138 151 129 162 L130 149 Q124 165 115 169 L115 154 Q108 170 102 171 L99 156 Q93 170 85 167 L86 153 Q76 165 69 157 L73 143 Q64 151 61 143 Q68 132 65 114 Z"/>
        <g id="hatGroup" transform="translate(0 -4)">
          <g id="hatCrown">
            <path class="hat-cone" d="M58 91 Q69 68 76 42 Q81 20 86 6 Q96 21 111 24 Q128 28 142 17 Q139 31 129 39 Q135 59 141 86 Q103 79 58 91 Z"/>
            <path class="hat-fold" d="M87 9 Q98 34 88 78 M114 26 Q108 43 114 75 M132 27 Q125 42 127 65"/>
            <path class="hat-highlight" d="M84 16 Q82 39 72 70 M91 11 Q102 28 114 30"/>
            <path class="hat-patch" d="M98 43 l15 2-1 15-16-3z"/>
            <path class="hat-stitch" d="M100 45 l3 .6 M108 47 l3 .5 M97 52 l3 .6 M106 55 l4 .5 M111 51 l2 .3"/>
            <g id="hatCharm">
              <path class="charm-string" d="M138 20 Q150 28 142 41"/>
              <circle class="charm-ring" cx="140" cy="21" r="2"/>
              <path class="moon-charm" d="M141 38 A8 8 0 1 0 147 50 A6 6 0 1 1 141 38 Z"/>
            </g>
          </g>
          <path class="hat-band" d="M60 78 Q99 69 139 78 L142 89 Q101 80 57 91 Z"/>
          <rect class="hat-buckle" x="106" y="75" width="11" height="10" rx="1.5" transform="rotate(4 111.5 80)"/>
          <rect class="hat-buckle-hole" x="109" y="77.5" width="5" height="5" rx="0.6" transform="rotate(4 111.5 80)"/>
          <path class="hat-brim" d="M40 91 Q54 76 78 80 Q99 86 120 79 Q145 74 160 90 Q151 103 124 102 Q101 99 78 103 Q52 104 40 91 Z"/>
          <path class="brim-highlight" d="M48 89 Q72 82 94 90 Q122 82 152 89"/>
          <path class="brim-edge" d="M43 94 Q62 102 81 98 Q102 94 122 98 Q144 101 157 92"/>
        </g>

        <ellipse class="ear" cx="68" cy="130" rx="5.5" ry="8"/>
        <ellipse class="ear" cx="132" cy="130" rx="5.5" ry="8"/>
        <ellipse class="face" cx="100" cy="128" rx="32" ry="32.5"/>
        <path class="face-shadow" d="M122 103 Q135 126 124 149 Q116 160 102 160 Q121 147 122 127 Q123 112 114 101 Z"/>
        <path class="hair-side hair-left" d="M72 104 Q62 115 67 129 Q62 139 68 148 L75 143 Q70 155 79 160 L83 149 Q80 161 88 164 L91 151 Q83 142 82 127 Q82 112 89 101 Z"/>
        <path class="hair-side hair-right" d="M128 103 Q138 114 133 128 Q140 139 133 149 L126 143 Q131 156 122 161 L117 149 Q120 162 112 165 L109 151 Q118 141 118 126 Q118 112 111 101 Z"/>
        <path class="hair-highlight" d="M73 109 Q68 127 75 142 M79 104 Q74 126 82 150 M126 108 Q132 126 125 143 M120 104 Q126 126 118 151"/>
        <path class="hat-seat-shadow" d="M65 95 Q100 105 135 94 Q127 104 100 106 Q73 104 65 95 Z"/>
        <path class="hat-brim-front" transform="translate(0 -4)" d="M61 93 Q80 102 100 97 Q121 102 140 92 Q128 103 102 104 Q76 105 61 93 Z"/>

        <path class="brow" d="M76 116 Q85 111 93 116"/>
        <path class="brow" d="M107 116 Q116 111 124 116"/>
        <g id="eyes">
          <g id="pupils">
            <ellipse class="pupil" cx="85" cy="128" rx="7" ry="10.2"/>
            <ellipse class="pupil" cx="115" cy="128" rx="7" ry="10.2"/>
            <circle class="eye-glint eye-glint-large" cx="82.8" cy="123.8" r="2.1"/>
            <circle class="eye-glint eye-glint-large" cx="112.8" cy="123.8" r="2.1"/>
            <circle class="eye-glint eye-glint-small" cx="88.3" cy="132.5" r="0.9"/>
            <circle class="eye-glint eye-glint-small" cx="118.3" cy="132.5" r="0.9"/>
          </g>
        </g>
        <g class="freckles">
          <circle cx="76" cy="142" r="0.8"/><circle cx="80" cy="144" r="0.65"/>
        </g>
        <path class="scar" d="M121 139 l-3 5 M125 140 l-4 6"/>
        <path class="nose" d="M100 130 Q97 137 101 138 Q104 138 105 135"/>
        <path id="closedSmile" class="closed-smile" d="M92 148 Q100 154 108 148"/>
        <g id="mouthRig">
          <ellipse id="mouth" class="mouth-cavity" cx="100" cy="149" rx="9" ry="6.8"/>
          <path id="teeth" class="teeth" d="M92 145 Q100 141 108 145 L107 147 Q100 149 93 147 Z"/>
          <ellipse id="tongue" class="tongue" cx="100" cy="153" rx="5.5" ry="2.7"/>
          <path class="tongue-line" d="M100 151 Q98 152 99 154"/>
        </g>

        <path class="neck-shadow" d="M87 154 Q100 163 113 154 Q109 167 100 170 Q91 167 87 154 Z"/>

        <!-- Duct tape over the mouth while paused (slap on / rip off via CSS) -->
        <g transform="rotate(-6 100 144)">
          <g id="tape">
            <rect class="tape-base" x="80" y="137" width="40" height="16" rx="2"/>
            <line class="tape-line" x1="84" y1="142" x2="116" y2="142"/>
            <line class="tape-line" x1="84" y1="148" x2="115" y2="148"/>
            <path class="tape-edge" d="M80 139 l-4 3 4 4 z"/>
            <path class="tape-edge" d="M120 151 l4 -3 -4 -4 z"/>
          </g>
        </g>
      </g>

      <g id="gear">
        <path class="strap strap-left" d="M78 158 Q99 178 123 205"/>
        <path class="strap strap-right" d="M122 158 Q101 180 82 204"/>
        <path class="strap-stitch" d="M82 160 Q101 178 120 201 M118 160 Q100 180 85 201"/>
        <path class="belt" d="M72 195 Q100 202 130 195"/>
        <rect class="belt-buckle" x="94" y="194" width="13" height="10" rx="1.8"/>
        <rect class="buckle-hole" x="97" y="196.5" width="7" height="5" rx="0.8"/>
        <path class="satchel" d="M117 185 Q133 181 143 191 L140 215 Q128 222 115 213 Z"/>
        <path class="satchel-flap" d="M117 190 Q130 183 142 191 L137 201 Q127 205 116 198 Z"/>
        <path class="satchel-stitch" d="M119 194 Q129 199 138 195 M119 211 Q128 216 137 211"/>
        <circle class="satchel-stud" cx="129" cy="199" r="2.1"/>
        <g class="vial vial-green">
          <path class="vial-glass" d="M72 187 L79 188 L78 192 Q84 199 79 207 Q71 211 67 204 Q66 198 72 192 Z"/>
          <path class="vial-liquid" d="M69 199 Q75 196 81 200 Q82 205 78 207 Q71 209 69 204 Z"/>
          <rect class="vial-cork" x="72" y="184" width="7" height="5" rx="1" transform="rotate(5 75.5 186.5)"/>
          <path class="vial-shine" d="M72 193 Q69 198 71 202"/>
        </g>
        <g class="vial vial-blue">
          <path class="vial-glass" d="M84 187 L91 187 L91 192 Q97 198 93 206 Q87 211 81 206 Q78 200 84 192 Z"/>
          <path class="vial-liquid" d="M81 199 Q87 196 94 199 Q95 204 92 207 Q85 210 82 205 Z"/>
          <rect class="vial-cork" x="84" y="183.5" width="7" height="5" rx="1"/>
          <path class="vial-shine" d="M85 193 Q82 198 84 202"/>
        </g>
      </g>
    </g>
  </g>

  <g id="sparkles">
    <path class="sparkle sparkle-one" d="M43 139 l1.5 4 4 1.5-4 1.5-1.5 4-1.5-4-4-1.5 4-1.5z"/>
    <path class="sparkle sparkle-two" d="M156 159 l1 3 3 1-3 1-1 3-1-3-3-1 3-1z"/>
    <path class="leaf-mote" d="M154 125 Q161 120 163 126 Q160 132 154 125 Z"/>
  </g>
  <!-- Thought bubble: shown via the .thinking class while the model works -->
  <g id="thoughtBubble">
    <circle class="tb-trail" cx="133" cy="75" r="3"/>
    <circle class="tb-trail" cx="143" cy="66" r="4"/>
    <path class="tb-cloud" d="M135 48 Q133 37 143 34 Q149 25 158 33 Q169 34 166 45 Q171 53 161 58 Q150 63 141 57 Q133 57 135 48 Z"/>
    <path class="tb-rune" d="M146 46 q5-7 10 0 q-5 7-10 0 M157 41 l2 9"/>
  </g>
</svg>`;
    this.mouthRig = this.stage.querySelector("#mouthRig");
    this.closedSmile = this.stage.querySelector("#closedSmile");
    this.tongue = this.stage.querySelector("#tongue");
    this.teeth = this.stage.querySelector("#teeth");
    this.eyes = this.stage.querySelector("#eyes");
    this._applyMouth(0);
    this._scheduleBlink();
    this._scheduleGlance();
    this._scheduleGesture();
  }

  setMouthOpen(v) {
    if (!this.mouthRig) return;
    this.mouthTarget = Math.max(0, Math.min(1, Number(v) || 0));
    if (this.reducedMotion) {
      this.mouthValue = this.mouthTarget;
      this._applyMouth(this.mouthValue);
      return;
    }
    if (!this.mouthRaf) {
      this.mouthTickAt = performance.now();
      this.mouthRaf = requestAnimationFrame((now) => this._tickMouth(now));
    }
  }

  setTalking(on) {
    this.talking = !!on;
    this.stage.classList.toggle("talking", this.talking);
    this._rescheduleGesture(550);
  }

  setThinking(on) {
    this.thinking = !!on;
    this.stage.classList.toggle("thinking", this.thinking);
    this._rescheduleGesture(850);
  }

  setPaused(on) {
    this.paused = !!on;
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
    this._rescheduleGesture(1000);
  }

  _applyMouth(v) {
    if (!this.mouthRig) return;
    const expressive = Math.pow(Math.max(0, Math.min(1, v)), 0.72);
    const openMix = Math.max(0, Math.min(1, (expressive - 0.04) * 4));
    const sx = 0.9 + expressive * 0.16;
    const sy = 0.28 + expressive * 1.05;
    this.mouthRig.style.opacity = String(openMix);
    this.mouthRig.style.transform = `scale(${sx.toFixed(3)}, ${sy.toFixed(3)})`;
    if (this.closedSmile) this.closedSmile.style.opacity = String(1 - openMix);
    if (this.tongue) this.tongue.style.opacity = String(Math.max(0, Math.min(1, (expressive - 0.2) * 1.65)));
    if (this.teeth) this.teeth.style.opacity = String(Math.max(0, Math.min(1, expressive * 1.4)));
  }

  _tickMouth(now) {
    const dt = Math.min(50, Math.max(1, now - this.mouthTickAt));
    this.mouthTickAt = now;
    const tau = this.mouthTarget > this.mouthValue ? 46 : 72;
    const blend = 1 - Math.exp(-dt / tau);
    this.mouthValue += (this.mouthTarget - this.mouthValue) * blend;
    if (Math.abs(this.mouthTarget - this.mouthValue) < 0.002) {
      this.mouthValue = this.mouthTarget;
      this._applyMouth(this.mouthValue);
      this.mouthRaf = 0;
      return;
    }
    this._applyMouth(this.mouthValue);
    this.mouthRaf = requestAnimationFrame((next) => this._tickMouth(next));
  }

  _scheduleBlink(delay = 2400 + Math.random() * 3600) {
    clearTimeout(this.blinkTimer);
    this.blinkTimer = setTimeout(() => {
      if (!this.eyes) return;
      const doubleBlink = Math.random() < 0.2;
      this.eyes.classList.add("blink");
      this.blinkResetTimer = setTimeout(() => {
        if (!this.eyes) return;
        this.eyes.classList.remove("blink");
        if (doubleBlink) {
          this.doubleBlinkTimer = setTimeout(() => {
            if (!this.eyes) return;
            this.eyes.classList.add("blink");
            this.blinkResetTimer = setTimeout(
              () => this.eyes?.classList.remove("blink"),
              95
            );
          }, 145);
        }
      }, 105);
      this._scheduleBlink(2600 + Math.random() * 4400);
    }, delay);
  }

  _scheduleGlance(delay = 1900 + Math.random() * 3200) {
    clearTimeout(this.glanceTimer);
    if (this.reducedMotion) return;
    this.glanceTimer = setTimeout(() => {
      if (!this.stage.isConnected) return;
      const looks = [
        [-1.8, -0.6], [1.8, -0.5], [-1.4, 0.8], [1.4, 0.7], [0, -1.1],
      ];
      const [x, y] = looks[Math.floor(Math.random() * looks.length)];
      this.stage.style.setProperty("--look-x", `${x}px`);
      this.stage.style.setProperty("--look-y", `${y}px`);
      clearTimeout(this.glanceResetTimer);
      this.glanceResetTimer = setTimeout(() => {
        this.stage.style.setProperty("--look-x", "0px");
        this.stage.style.setProperty("--look-y", "0px");
      }, 700 + Math.random() * 1100);
      this._scheduleGlance(2500 + Math.random() * 4200);
    }, delay);
  }

  _clearGesture() {
    this.stage.classList.remove(
      "gesture-wave",
      "gesture-perk",
      "gesture-swish",
      "gesture-emphasis",
      "gesture-nod",
      "gesture-think-tap"
    );
  }

  _rescheduleGesture(delay) {
    clearTimeout(this.gestureTimer);
    clearTimeout(this.gestureResetTimer);
    this._clearGesture();
    if (!this.reducedMotion) this._scheduleGesture(delay);
  }

  _scheduleGesture(delay) {
    clearTimeout(this.gestureTimer);
    if (this.reducedMotion) return;
    const wait = delay ?? (this.talking
      ? 1300 + Math.random() * 1800
      : this.thinking
        ? 2600 + Math.random() * 2300
        : 4200 + Math.random() * 4200);
    this.gestureTimer = setTimeout(() => {
      if (!this.stage.isConnected || this.paused) {
        this._scheduleGesture(1800);
        return;
      }
      const options = this.talking
        ? ["gesture-emphasis", "gesture-nod", "gesture-wave"]
        : this.thinking
          ? ["gesture-think-tap", "gesture-perk"]
          : ["gesture-wave", "gesture-perk", "gesture-swish"];
      const name = options[Math.floor(Math.random() * options.length)];
      const duration = name === "gesture-wave" ? 1120 : name === "gesture-swish" ? 1250 : 900;
      this._clearGesture();
      this.stage.classList.add(name);
      this.gestureResetTimer = setTimeout(() => {
        this._clearGesture();
        this._scheduleGesture();
      }, duration);
    }, wait);
  }

  destroy() {
    cancelAnimationFrame(this.mouthRaf);
    clearTimeout(this.blinkTimer);
    clearTimeout(this.blinkResetTimer);
    clearTimeout(this.doubleBlinkTimer);
    clearTimeout(this.glanceTimer);
    clearTimeout(this.glanceResetTimer);
    clearTimeout(this.gestureTimer);
    clearTimeout(this.gestureResetTimer);
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
