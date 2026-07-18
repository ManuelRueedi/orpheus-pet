// Web Audio lip-sync engine.
//
// Plays a WAV ArrayBuffer, measures loudness (RMS) in real time, and reports a
// smoothed 0..1 "mouth open" level via the onLevel callback. Because the pet
// owns the audio buffer, the mouth is always perfectly in sync with playback.
export class LipSync {
  constructor({ onLevel = () => {}, gain = 3.5 } = {}) {
    this.onLevel = onLevel;
    this.gain = gain;
    this.ctx = null;
    this.analyser = null;
    this.timeBuf = null;
    this.source = null;
    this.raf = 0;
  }

  _ensure() {
    if (this.ctx) return;
    const Ctx = window.AudioContext || window.webkitAudioContext;
    this.ctx = new Ctx();
    this.analyser = this.ctx.createAnalyser();
    this.analyser.fftSize = 1024;
    this.analyser.smoothingTimeConstant = 0.6;
    this.timeBuf = new Uint8Array(this.analyser.fftSize);
    // Analyser -> destination so we also hear the audio.
    this.analyser.connect(this.ctx.destination);
  }

  async _resume() {
    this._ensure();
    if (this.ctx.state === "suspended") await this.ctx.resume();
  }

  _level() {
    this.analyser.getByteTimeDomainData(this.timeBuf);
    let sum = 0;
    for (let i = 0; i < this.timeBuf.length; i++) {
      const v = (this.timeBuf[i] - 128) / 128;
      sum += v * v;
    }
    const rms = Math.sqrt(sum / this.timeBuf.length);
    return Math.max(0, Math.min(1, rms * this.gain));
  }

  _startLoop() {
    if (this.raf) return; // already running
    const tick = () => {
      this.onLevel(this._level());
      this.raf = requestAnimationFrame(tick);
    };
    this.raf = requestAnimationFrame(tick);
  }

  _stopLoop() {
    if (this.raf) cancelAnimationFrame(this.raf);
    this.raf = 0;
  }

  _stopSource() {
    if (this.source) {
      try { this.source.stop(); } catch { /* already stopped */ }
      try { this.source.disconnect(); } catch { /* noop */ }
      this.source = null;
    }
  }

  // Play a WAV/audio ArrayBuffer and drive lip-sync until it finishes.
  async play(arrayBuffer) {
    await this._resume();
    // decodeAudioData detaches the buffer; copy so the caller can reuse theirs.
    const audioBuffer = await this.ctx.decodeAudioData(arrayBuffer.slice(0));
    this._stopSource();
    const src = this.ctx.createBufferSource();
    src.buffer = audioBuffer;
    src.connect(this.analyser);
    this.source = src;
    const ended = new Promise((resolve) => { src.onended = resolve; });
    src.start();
    this._startLoop();
    await ended;
    this._stopLoop();
    this._stopSource();
    this.onLevel(0);
  }

  // Progressively play a chunked-WAV Response (from synthesizeStream).
  // Each PCM chunk is scheduled back-to-back on the AudioContext timeline for
  // gapless playback, routed through the analyser so lip-sync starts with the
  // very first chunk. onStart fires when the first audio is scheduled.
  async playStream(response, { sampleRate = 24000, onStart } = {}) {
    await this._resume();
    this._stopSource();
    const abort = { stopped: false };
    this._streamAbort = abort;
    const ctx = this.ctx;
    const reader = response.body.getReader();
    this._streamReader = reader;
    const active = new Set();
    this._streamSources = active;

    const HEADER = 44; // fixed-size header our stream endpoint emits
    let skipped = 0;
    let carry = new Uint8Array(0); // odd trailing byte between reads
    let nextTime = 0;
    let totalSamples = 0;

    const schedule = (bytes) => {
      // Copy to an aligned buffer — Int16Array views need even byteOffset.
      const pcm = new Int16Array(
        bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength)
      );
      if (!pcm.length) return;
      const buf = ctx.createBuffer(1, pcm.length, sampleRate);
      const ch = buf.getChannelData(0);
      for (let i = 0; i < pcm.length; i++) ch[i] = pcm[i] / 32768;
      const src = ctx.createBufferSource();
      src.buffer = buf;
      src.connect(this.analyser);
      // Small priming offset on the first chunk absorbs scheduling jitter.
      const startAt = Math.max(nextTime, ctx.currentTime + (totalSamples === 0 ? 0.12 : 0.02));
      if (totalSamples === 0) {
        this._startLoop();
        if (onStart) onStart();
      }
      src.start(startAt);
      nextTime = startAt + buf.duration;
      totalSamples += pcm.length;
      active.add(src);
      src.onended = () => active.delete(src);
    };

    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (abort.stopped) return;
        if (done) break;
        let data = value;
        if (skipped < HEADER) {
          const need = HEADER - skipped;
          if (data.length <= need) {
            skipped += data.length;
            continue;
          }
          data = data.subarray(need);
          skipped = HEADER;
        }
        const merged = new Uint8Array(carry.length + data.length);
        merged.set(carry);
        merged.set(data, carry.length);
        const even = merged.length & ~1;
        carry = merged.subarray(even);
        if (even) schedule(merged.subarray(0, even));
      }
    } finally {
      this._streamReader = null;
    }

    if (abort.stopped) return;
    if (totalSamples === 0) {
      this._stopLoop();
      this.onLevel(0);
      throw new Error("empty audio from engine (inference backend unreachable?)");
    }
    // Let the scheduled tail play out. Pause-aware: currentTime freezes while
    // the context is suspended, so this naturally waits through pauses.
    while (!abort.stopped && ctx.currentTime < nextTime + 0.05) {
      await new Promise((r) => setTimeout(r, 100));
    }
    this._stopLoop();
    this.onLevel(0);
  }

  // Pause playback by suspending the whole AudioContext: every scheduled
  // chunk freezes in place and later resumes exactly where it stopped, while
  // a running stream keeps buffering ahead on the frozen timeline.
  async pause() {
    if (!this.ctx || this.ctx.state !== "running") return false;
    await this.ctx.suspend();
    this._stopLoop();
    this.onLevel(0);
    return true;
  }

  async resume() {
    if (!this.ctx || this.ctx.state !== "suspended") return false;
    await this.ctx.resume();
    this._startLoop();
    return true;
  }

  stop() {
    if (this._streamAbort) this._streamAbort.stopped = true;
    if (this._streamReader) {
      try { this._streamReader.cancel(); } catch { /* already closed */ }
      this._streamReader = null;
    }
    if (this._streamSources) {
      for (const s of this._streamSources) {
        try { s.stop(); } catch { /* already stopped */ }
      }
      this._streamSources.clear();
    }
    // A stop while paused must unfreeze the context so the source stops take
    // effect and the next session starts on a running clock.
    if (this.ctx && this.ctx.state === "suspended") {
      this.ctx.resume().catch(() => { /* context torn down */ });
    }
    this._stopLoop();
    this._stopSource();
    this.onLevel(0);
  }
}
