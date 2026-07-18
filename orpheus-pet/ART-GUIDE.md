# Orpheus Pet — Art Guide (Tripo → Rive)

This is the "make the actual witch" half of the project. The app already runs
with a placeholder SVG witch; follow this to replace it with your Tripo-designed,
Rive-rigged pet. **The code loads whatever you drop at `public/pets/witch.riv`** —
if that file exists it's used, otherwise the SVG fallback renders.

> Reminder on why it's this pipeline: Tripo only outputs **3D**. In a 2D
> (Rive) pet, Tripo is the **character designer** — it gives you a consistent,
> re-poseable witch you render to 2D art. The talking/lip-sync is done in Rive +
> the app, not by Tripo.

---

## The contract (what the code expects)

Your Rive file **must** match these names exactly, or lip-sync/animation won't bind.
Defined in [`src/pet/renderer.js`](src/pet/renderer.js):

| Thing | Name | Type | Range / meaning |
|---|---|---|---|
| State machine | `pet` | — | The one the app plays |
| Input | `mouthOpen` | **Number** | `0` = mouth closed → `100` = mouth fully open (drives lip-sync) |
| Input | `talking` | **Boolean** | `true` while a voice is playing (idle ↔ talk body motion) |
| Input | `thinking` | **Boolean** *(optional)* | `true` while the model is generating and no audio has arrived — a pondering pose (e.g. thought bubble, tapping chin). Skipped silently if the rig doesn't have it. |
| Input | `paused` | **Boolean** *(optional)* | `true` while playback is paused (user clicked the pet mid-speech). The fallback witch slaps **duct tape** over her mouth and rips it off on resume — rig your own gag/pose. Skipped silently if absent. |

The app sets `mouthOpen` every animation frame from the live audio loudness, and
flips `talking` on/off around playback. Everything else (blinking, idle float,
hat bob) is yours to animate freely inside the state machine.

---

## Part A — Generate the hero witch in Tripo

1. Go to **studio.tripo3d.ai** → new **Text to 3D** (or Image to 3D if you have a
   reference sketch).
2. Prompt (tweak to taste):

   > *A cute chibi witch character, big friendly eyes, small body, oversized
   > pointed purple wizard hat with a golden star, flowing purple cloak, rosy
   > cheeks, gentle smile, front-facing T-pose, symmetrical, clean solid
   > background, stylized cartoon, soft cel shading, full body, game-ready.*

   Keep it **front-facing, symmetrical, neutral pose** — that's what rigs cleanly
   into 2D. Avoid dramatic angles or props crossing the body.
3. Settings: Model **v3.0/v3.1**, **Stylized/cartoon** style, texture on. Generate
   a few, pick the cleanest silhouette (readable hat, clear face, separable arms).
4. (Optional) Use Tripo's **rig + a preset idle animation** just to preview it in
   motion — but for the 2D pet you don't export the 3D animation; you export views.

### If you meant a *familiar* (cat/owl/toad) instead of a little witch
Swap the prompt subject (e.g. *"a cute black cat familiar with a tiny witch hat,
big eyes, front-facing, symmetrical…"*). Same rules. Note a face with a clear
**mouth** area rigs best for lip-sync — an owl/cat can still work with a beak/mouth
morph.

---

## Part B — Get clean 2D art out of Tripo for Rive

Rive is vector/mesh 2D, so you need flat images to trace or import:

1. In Tripo's viewer, frame the model **dead-on front**, neutral lighting, and
   capture high-res screenshots (or use **Export → render/turnaround** if
   available). Grab at least:
   - **Neutral** (mouth closed) — the base.
   - **Mouth open** — rotate/pose the jaw or just note the mouth shape; this guides
     your open-mouth drawing.
   - A **¾ view** for reference (optional, for depth cues).
2. If you want crisp separable parts, import the front render into an editor and
   cut the pieces onto layers: **hat, hair, face, eyes (L/R), pupils, mouth,
   cloak, arms, hands**. Export as PNGs (transparent) or an SVG.
   - Tip: keep the **mouth** on its own layer with a bit of dark "inner mouth"
     behind it, so opening it looks like a real mouth, not a moved shape.

---

## Part C — Rig it in Rive (rive.app)

1. New file → **import** your PNGs/SVG onto an artboard (~**440×560**, matches the
   canvas the app creates). Position the parts into the witch.
2. Set pivots and bones/groups so parts move naturally (hat slightly above head,
   arms from shoulders). A **Mesh** on the cloak/hat gives nice squash & bob.
3. **Mouth (the important one):**
   - Simplest: put a mouth shape on its own group and animate its **vertical
     scale / open height** from closed → open across a timeline.
   - Nicer: a small mesh you deform from a thin line (closed) to an open oval.
4. Create a **State Machine** and **rename it `pet`** (exact).
   Add inputs:
   - **Number `mouthOpen`** (exact name).
   - **Boolean `talking`** (exact name).
5. Wire the inputs:
   - Add a **1D Blend State** (or a timeline mapped to the input) driven by
     `mouthOpen`, blending **closed-mouth (0) → open-mouth (100)**. This is the
     lip-sync.
   - Two states for the body: **Idle** (gentle float/blink) and **Talk** (a little
     sway/bounce), with transitions gated on `talking == true / false`.
6. **Preview** in Rive: drag `mouthOpen` 0→100 (mouth should open), toggle
   `talking` (body should switch to the sway). If that works in Rive, it works in
   the app.

### Export
- **Export → Runtime (.riv)**, include the `pet` state machine.
- Save it as **`orpheus-pet/public/pets/witch.riv`**.
- Restart `pnpm tauri dev` (or just reload) — the app auto-detects the file and
  swaps the SVG placeholder for your Rive witch. Nothing else to change.

---

## Part D — Per-voice skins (later; task #7)

You have **25 voices**. You almost certainly do **not** want 25 separate rigs.
The plan for the batching step:

- Keep **one rig** (`witch.riv`). Vary the **look** per voice: hat color, cloak
  palette, hair, a small accessory or familiar — ideally via Rive **theming /
  swappable artboards / a `skin` input**, or by exporting a few `.riv` variants
  named per voice (e.g. `pets/tara.riv`, `pets/leo.riv`).
- Tripo's role in batching: re-generate or recolor the hero to keep every variant
  **on-model**, then render → re-skin. The Tripo **API** (text→model→render) can
  automate producing the reference art for each variant.
- The app will then map the selected Orpheus voice → its skin. (That mapping and
  the Tripo API script are task #7; the loader already keys off the filename.)

---

## Quick reference

- Drop file here → `public/pets/witch.riv`
- State machine → `pet`
- Number input → `mouthOpen` (0–100)
- Boolean input → `talking`
- Canvas size the app uses → 440×560 (Fit: contain, centered)
- No file present → SVG fallback witch (already working)
