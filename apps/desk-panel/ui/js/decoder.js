// H.264 Annex-B -> WebCodecs VideoDecoder (hardware first) -> canvas.

const FRAME_HEADER_LEN = 9; // keyframe u8 + ts_us u64 LE
const MAX_QUEUE = 8;
const STATS_WINDOW_MS = 1000;

/** Splits a binary frame message from Rust into its parts. */
export function parseFrame(buffer) {
  const bytes = buffer instanceof ArrayBuffer ? new Uint8Array(buffer) : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return {
    keyframe: view.getUint8(0) === 1,
    tsUs: Number(view.getBigUint64(1, true)),
    data: bytes.subarray(FRAME_HEADER_LEN),
  };
}

/**
 * Decodes frames onto a canvas and tracks fps and decode latency.
 * Callbacks: onFirstFrame(), onNeedKeyframe(), onStats({fps, decodeMs}), onUnsupported().
 */
export class VideoPipeline {
  constructor(canvas, callbacks) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d", { alpha: false, desynchronized: true });
    this.cb = callbacks;
    this.decoder = null;
    this.hardware = null;
    this.needKey = true;
    this.submitted = new Map();
    this.shown = 0;
    this.decodeSum = 0;
    this.decodeCount = 0;
    this.firstFrame = false;
    this.timer = setInterval(() => this.flushStats(), STATS_WINDOW_MS);
  }

  /** Opens a decoder for the announced stream; tries hardware, then any decoder. */
  async configure(codec) {
    this.closeDecoder();
    if (typeof VideoDecoder === "undefined") {
      this.cb.onUnsupported();
      return;
    }
    const base = { codec, optimizeForLatency: true, avc: { format: "annexb" } };
    let chosen = null;
    for (const preference of ["prefer-hardware", "no-preference"]) {
      const candidate = { ...base, hardwareAcceleration: preference };
      const support = await VideoDecoder.isConfigSupported(candidate).catch(() => ({ supported: false }));
      if (support.supported) {
        chosen = candidate;
        this.hardware = preference === "prefer-hardware";
        break;
      }
    }
    if (!chosen) {
      this.cb.onUnsupported();
      return;
    }
    this.decoder = new VideoDecoder({
      output: (frame) => this.draw(frame),
      error: () => {
        this.needKey = true;
        this.cb.onNeedKeyframe();
      },
    });
    this.decoder.configure(chosen);
    this.needKey = true;
  }

  /** Feeds one access unit; delta frames are dropped until a keyframe arrives. */
  push({ keyframe, tsUs, data }) {
    const decoder = this.decoder;
    if (!decoder || decoder.state !== "configured") return;
    if (this.needKey && !keyframe) return;
    if (decoder.decodeQueueSize > MAX_QUEUE && !keyframe) {
      this.needKey = true;
      this.cb.onNeedKeyframe();
      return;
    }
    this.needKey = false;
    this.submitted.set(tsUs, performance.now());
    try {
      decoder.decode(new EncodedVideoChunk({ type: keyframe ? "key" : "delta", timestamp: tsUs, data }));
    } catch {
      this.needKey = true;
      this.cb.onNeedKeyframe();
    }
  }

  draw(frame) {
    const started = this.submitted.get(frame.timestamp);
    if (started !== undefined) {
      this.decodeSum += performance.now() - started;
      this.decodeCount += 1;
      this.submitted.delete(frame.timestamp);
    }
    if (this.canvas.width !== frame.displayWidth || this.canvas.height !== frame.displayHeight) {
      this.canvas.width = frame.displayWidth;
      this.canvas.height = frame.displayHeight;
    }
    this.ctx.drawImage(frame, 0, 0);
    frame.close();
    this.shown += 1;
    if (!this.firstFrame) {
      this.firstFrame = true;
      this.cb.onFirstFrame();
    }
  }

  flushStats() {
    const decodeMs = this.decodeCount ? this.decodeSum / this.decodeCount : 0;
    this.cb.onStats({ fps: this.shown, decodeMs });
    this.shown = 0;
    this.decodeSum = 0;
    this.decodeCount = 0;
    if (this.submitted.size > 120) this.submitted.clear();
  }

  closeDecoder() {
    if (this.decoder && this.decoder.state !== "closed") this.decoder.close();
    this.decoder = null;
    this.submitted.clear();
  }

  close() {
    clearInterval(this.timer);
    this.closeDecoder();
  }
}
