# Orpheus Pet 🧙‍♀️

An animated **desktop pet witch** that acts as the face of [Orpheus-FastAPI](../Orpheus-FastAPI).
It floats on your desktop (transparent, always-on-top), you type something, it asks
Orpheus to speak in the chosen voice, and it **lip-syncs** to the audio.

Built with **Tauri v2** (Rust shell) + **Rive** (2D animation) + the **Web Audio API**.

## Run it — one program

Prereqs + full setup (Python backend, llama-server, config) are in the
[root README](../README.md). Short version, from `orpheus-pet/`:

```bash
pnpm install
pnpm tauri dev      # dev window with hot-reload
pnpm tauri build    # produce a packaged .exe/installer
```

That's the only thing you launch. On startup the pet **spawns the whole voice
stack itself** as hidden child processes ([`src-tauri/src/stack.rs`](src-tauri/src/stack.rs)):

1. `llama-server.exe` with the Orpheus GGUF on the GPU (`:1234`)
2. Orpheus-FastAPI via the venv's uvicorn (`:5005`)

Paths and ports live in [`stack.config.json`](stack.config.json); child logs go
to `C:\Dev\OrpheusTTS\logs\`. If a port is already listening the pet **reuses**
that server instead of double-spawning, and on **Quit** (tray menu) it kills only
the processes it spawned. While the engine boots the status shows
"starting voice engine…" (~10–20s, model load included). While a request is
generating and no audio has arrived yet, the witch shows a **thinking pose**
(thought bubble + sway) that switches to talking on the first audio chunk.

Note for `pnpm tauri dev`: a Rust rebuild hard-kills the previous instance, so
spawned servers survive into the next instance — which then just reuses them.
Quit from the tray (or `taskkill` llama-server/python) for a full teardown.

## Interactions

- **Drag** the witch to move her anywhere on screen.
- **Left-click her when idle** and she says a random sample line (see
  [`src/pet/samples.js`](src/pet/samples.js)) — a quick way to hear a voice.
  Every spoken snippet (samples, voice-change greetings, hello/goodbye) is
  **localized to the selected voice's language** (EN/FR/DE/ES/IT/KO/HI/ZH),
  falling back to English.
- **Right-click** her to open/close the speech panel. The window is just the
  witch when closed; opening grows it toward whichever side has screen room
  (below/above/left/right), keeping the witch anchored.
- **Drag her off a screen edge** and she springs back into view. The panel's **Language**
  dropdown filters the voice list to that language and defaults on first run to
  your **Windows display language** (via the OS locale), falling back to English.
- **Left-click while she's talking** to pause — the audio freezes mid-word and
  she gets **duct tape** slapped over her mouth. Click again to rip it off and
  resume exactly where she stopped. (Streaming keeps buffering while paused.)
- **New input always wins**: pressing Speak or the global hotkey cancels any
  current or paused speech and starts fresh.

## Languages & voice models

Each Orpheus language is a **separate ~3.5 GB fine-tuned model** (the base model
is English-only), loaded one at a time. The panel's **Language** dropdown lists
all supported languages (EN/FR/DE/ES/IT/KO/HI/ZH). Picking one whose model isn't
downloaded yet **asks you to confirm the ~3.5 GB download**; then the picker area
becomes an in-panel **progress bar + Stop button** (with a filling cauldron over
the witch) while it fetches from `huggingface.co/lex-au/<model>` and hot-swaps
llama-server. **Stop** cancels the download. Downloaded models are cached and the
choice persists across restarts. Spanish & Italian share one model (instant
switch). Managed in [`src-tauri/src/stack.rs`](src-tauri/src/stack.rs)
(`set_language` / `model_status` / `cancel_download`).

## Read anything aloud — global hotkey

Highlight text in **any** app and press **Ctrl+Alt+A** (default). The witch pops
up and reads the selection in the current voice.

**Change it in the panel:** open the panel (right-click the witch), click the
hotkey button next to "Read-aloud hotkey", and press your combo — it re-registers
immediately (no restart) and is saved to [`stack.config.json`](stack.config.json).
You can also edit `hotkey` there directly. If a combo can't be registered the app
falls back to `Ctrl+Q` then `Ctrl+Alt+O` (the active one is logged as
`[hotkey] registered: …`). How it works
([`src-tauri/src/selection.rs`](src-tauri/src/selection.rs)): a simulated
Ctrl+C captures the selection, the clipboard is read and then **restored**.
If nothing is selected it falls back to the existing clipboard text, so
"Ctrl+C somewhere, then hotkey" works too. Caveats: apps running **as
administrator** ignore the simulated copy (Windows blocks injecting input into
elevated windows), and selections over ~1500 chars are trimmed.

## How it works

| Piece | File | Role |
|---|---|---|
| Window shell + tray | [`src-tauri/src/lib.rs`](src-tauri/src/lib.rs) | Transparent always-on-top overlay, bottom-right placement, tray (Show/Hide/Quit), close-to-tray |
| Window config | [`src-tauri/tauri.conf.json`](src-tauri/tauri.conf.json) | `transparent`, `decorations:false`, `alwaysOnTop`, `skipTaskbar` |
| Pet renderer | [`src/pet/renderer.js`](src/pet/renderer.js) | SVG fallback witch **and** Rive renderer behind one `setMouthOpen`/`setTalking` contract |
| Lip-sync | [`src/pet/lipsync.js`](src/pet/lipsync.js) | Plays the WAV, measures RMS loudness per frame → 0..1 mouth level |
| Orpheus client | [`src/pet/orpheus.js`](src/pet/orpheus.js) | `POST /v1/audio/speech`, `GET /v1/audio/voices` via the Tauri HTTP plugin (no CORS) |
| Orchestration | [`src/main.js`](src/main.js) | Wires UI, drag-to-move, click-to-talk |

**Why the HTTP plugin:** Orpheus has no CORS middleware, so a webview `fetch`
would be blocked. Requests go through Rust (`@tauri-apps/plugin-http`), scoped to
`localhost:5005` in [`capabilities/default.json`](src-tauri/capabilities/default.json).

## Getting the real witch in

The app renders a placeholder **SVG witch** until you provide Rive art. Drop a
`.riv` at `public/pets/witch.riv` and it's picked up automatically. The design →
rig pipeline (Tripo → Rive) and the exact input contract are in
[`ART-GUIDE.md`](ART-GUIDE.md).

Contract summary — state machine `pet`, Number `mouthOpen` (0–100), Boolean `talking`.

## Status / roadmap

- [x] Transparent overlay window + tray
- [x] SVG fallback pet with lip-sync (runnable with zero art)
- [x] Orpheus client + Web Audio lip-sync
- [ ] Drop in the Tripo-designed Rive witch (`public/pets/witch.riv`)
- [ ] Per-voice skins + Tripo API batch script (25 voices)
