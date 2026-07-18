# AGENTS.md — notes for the next agent 🧙‍♀️🤖

Hello, fellow automaton. This is a **local text-to-speech desktop pet** for
Windows. A transparent Tauri witch (`orpheus-pet/`) is the whole product; on
launch she spawns and owns her own backend. Read this before you start editing —
a few things here will bite you if you assume the usual.

## The one rule

**It's one program.** You run `orpheus-pet`; its Rust side
([`src-tauri/src/stack.rs`](orpheus-pet/src-tauri/src/stack.rs)) launches
`llama-server` (GGUF inference, `:1234`) and Orpheus-FastAPI (uvicorn, `:5005`)
as hidden children and kills them on Quit. Don't add a second launcher or start
those by hand — if a port's already up, the pet reuses it.

## Layout

```
OrpheusTTS/
├─ orpheus-pet/            ← THE APP (edit here 95% of the time)
│  ├─ index.html           pet window            panel.html  controls window
│  ├─ src/main.js          pet logic (anchor, audio, speak, drag, samples)
│  ├─ src/panel.js         controls window (voice/lang/hotkey/download UI)
│  ├─ src/pet/             renderer.js · lipsync.js · orpheus.js · samples.js
│  ├─ src-tauri/src/       lib.rs (window/tray/hotkey) · stack.rs (backend) · selection.rs
│  ├─ src-tauri/tauri.conf.json      window defs + bundle
│  ├─ src-tauri/capabilities/        permission grants (baked at build)
│  └─ stack.config.json    paths/ports/quant/hotkey  (git-ignored; auto-created from .example on first run)
├─ Orpheus-FastAPI/        ← vendored TTS server (Apache-2.0). token→WAV via SNAC.
├─ llama/    models/       ← binaries + GGUF weights (git-ignored, fetched on setup)
└─ logs/                   ← child-process logs
```

## Commands

```bash
# from orpheus-pet/
pnpm install
pnpm tauri dev      # run with hot-reload
pnpm tauri build    # bundle an installer/exe

# verify Rust WITHOUT a full run (fast, and safe while the app is running —
# `check` never relinks the locked .exe):
cargo check --manifest-path orpheus-pet/src-tauri/Cargo.toml
```

## Sharp edges (the important part)

- **Config/capability/`tauri.conf.json` changes need a FULL `pnpm tauri dev`
  restart.** They're read at startup / baked at build — a hot-reload won't pick
  them up. This includes adding a window, editing `capabilities/`, or the two
  windows' geometry.
- **Two windows, one bridge.** `main` = the pet (fixed 148×280, owns render +
  audio + `speak`), `panel` = the controls (hidden 352×292). They talk over
  events: panel→pet `ui:speak`/`ui:voice`/`ui:switching`/`ui:hide`/`ui:close`;
  pet→panel `pet:status`/`pet:panel-open`/`pet:ready`; Rust broadcasts
  `speak-selection`/`pet-visibility`/`model-progress`. Both share **one**
  capability (`windows:["main","panel"]`). Window geometry math is in **logical
  px** (physical ÷ scaleFactor).
- **`.env` is authoritative for Orpheus-FastAPI** (`load_dotenv(override=True)`
  beats process env). `stack.rs` rewrites `ORPHEUS_API_URL` → the managed llama
  port before every spawn. Don't hand-tune that line expecting it to survive.
- **No CORS on the FastAPI server.** The pet calls it through the Rust HTTP
  plugin (`@tauri-apps/plugin-http`), scoped to `:5005` in `capabilities/`. A
  webview `fetch()` will be blocked — don't "fix" it by switching to fetch.
- **uvicorn runs without `--reload`.** Editing `Orpheus-FastAPI/app.py` or
  `tts_engine/` does nothing until you kill the python process and relaunch the
  pet. (`/v1/audio/speech/stream` chunked-WAV streaming lives in `app.py`.)
- **Never delete `Orpheus-FastAPI/static` or `templates`.** `app.py` mounts them
  at import; `uvicorn app:app` won't start without them.
- **`PYTHONUTF8=1` is mandatory** (its emoji log lines crash on cp1252). `stack.rs`
  sets it when spawning; keep it.
- **Each language is a separate GGUF.** The base `Orpheus-3b-FT` is English-only;
  non-English voices on it produce *garbage* (a runaway ~9s clip for one line is
  the tell — byte-size ≠ good speech). `set_model_selection` downloads +
  hot-swaps an explicit language/quant pair; `set_language` and `set_quant`
  remain compatibility wrappers.
- **Model size / low-spec knobs** live in `stack.config.json`: `quant` (`Q8_0` →
  `Q4_K_M` → `Q2_K`, auto-falls-back to `Q8_0` when a quant isn't published) and
  `llamaArgs` (`-ngl` GPU layers). Relative paths in that file resolve against
  its own directory (`resolve_paths` in stack.rs) — keep them portable.
  The panel stages its language and size dropdowns as **one model target**. Both
  remain editable until the user confirms; `set_model_selection` (lib.rs →
  stack.rs) then loads/downloads exactly that pair, never an intermediate pair.
  Model operations carry an ID, target language/quant, terminal phase, and
  loaded-vs-preferred quant so late events cannot overwrite a newer switch.
  `model_status` and `installed_languages` accept an explicit `quant`; language
  availability must always describe the staged size while identifying the
  separately loaded pair.
- **Autostart is release-only** (`#[cfg(not(debug_assertions))]` in `lib.rs`).
  The dev binary must not self-register — it needs the Vite dev server.
- **Windows-only right now** (`taskkill`/`netstat`/`windows-sys`, `venv\Scripts`).
  Guard new OS-specific bits behind `#[cfg(windows)]` like the existing code.

## Conventions

- Match the surrounding style: **vanilla JS**, no framework, no build magic;
  idiomatic Rust. Comments explain *why*, not *what*.
- A fresh clone has **no models** — first language pick downloads one. Don't
  assume weights exist; guard on `Path::is_file()` (see `start()`).
- The Rive rig contract (for real art at `orpheus-pet/public/pets/witch.riv`):
  state machine `pet`, Number `mouthOpen` 0–100, Boolean `talking` (+ optional
  `thinking`, `paused`). Falls back to an SVG witch when absent. Full pipeline in
  [`orpheus-pet/ART-GUIDE.md`](orpheus-pet/ART-GUIDE.md).
- When you change Rust, `cargo check` it. When you change windows/capabilities,
  say so — the user has to restart `tauri dev`.
