// Client for the Orpheus-FastAPI TTS server.
//
// Requests go through the Tauri HTTP plugin (Rust side) rather than the
// webview's fetch, so we bypass CORS entirely — the Orpheus server has no
// CORS middleware, so a plain browser fetch from tauri://localhost would be
// blocked. This also keeps everything working in a packaged build.
import { fetch } from "@tauri-apps/plugin-http";

// 127.0.0.1 (not "localhost"): on Windows localhost can resolve to IPv6 ::1,
// but uvicorn binds IPv4 only — the explicit IP avoids a flaky first connection.
const DEFAULT_BASE = "http://127.0.0.1:5005";

// Mirrors Orpheus-FastAPI's AVAILABLE_VOICES so the picker still works when
// the server is offline (see tts_engine/inference.py).
export const FALLBACK_VOICES = [
  "tara", "leah", "jess", "leo", "dan", "mia", "zac", "zoe",
  "pierre", "amelie", "marie",
  "jana", "thomas", "max",
  "유나", "준서",
  "ऋतिका",
  "长乐", "白芷",
  "javi", "sergio", "maria",
  "pietro", "giulia", "carlo",
];

export function getBaseUrl() {
  return localStorage.getItem("orpheus.baseUrl") || DEFAULT_BASE;
}

export function setBaseUrl(url) {
  localStorage.setItem("orpheus.baseUrl", url);
}

// GET /v1/audio/voices -> { status: "ok", voices: [...] }
// Always resolves; falls back to the built-in list on any error.
export async function getVoices() {
  try {
    const res = await fetch(`${getBaseUrl()}/v1/audio/voices`, { method: "GET" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const data = await res.json();
    const voices = Array.isArray(data?.voices) ? data.voices : [];
    if (!voices.length) throw new Error("empty voice list");
    return { ok: true, voices };
  } catch (err) {
    return { ok: false, voices: FALLBACK_VOICES, error: String(err) };
  }
}

// POST /v1/audio/speech/stream -> chunked WAV that begins after ~7 tokens.
// Uses the webview's NATIVE fetch (not the Tauri plugin): we need the response
// body as a ReadableStream for progressive playback, and the server now sends
// CORS headers so the webview allows it.
export async function synthesizeStream(text, voice, { speed = 1.0 } = {}) {
  const res = await window.fetch(`${getBaseUrl()}/v1/audio/speech/stream`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "orpheus",
      input: text,
      voice,
      response_format: "wav",
      speed,
    }),
  });
  if (!res.ok || !res.body) {
    throw new Error(`stream endpoint failed: HTTP ${res.status}`);
  }
  return res;
}

// POST /v1/audio/speech -> WAV bytes (FileResponse, audio/wav).
// Returns an ArrayBuffer, or throws on error. Note this is synchronous on the
// server: it generates the whole clip before responding, so it can take a few
// seconds. Callers should show a "thinking" state while awaiting.
export async function synthesize(text, voice, { speed = 1.0 } = {}) {
  const res = await fetch(`${getBaseUrl()}/v1/audio/speech`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "orpheus",
      input: text,
      voice,
      response_format: "wav",
      speed,
    }),
  });
  if (!res.ok) {
    let detail = "";
    try { detail = (await res.text()).slice(0, 300); } catch { /* ignore */ }
    throw new Error(`Orpheus /v1/audio/speech failed: HTTP ${res.status} ${detail}`);
  }
  return await res.arrayBuffer();
}
