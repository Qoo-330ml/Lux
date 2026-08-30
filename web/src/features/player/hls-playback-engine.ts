import type { PlaybackEngine, PlaybackPerformance, PlaybackSnapshot } from "./playback-engine";
import type Hls from "hls.js";
export { canUseHls } from "./hls-capabilities";

const HLS_MIME = "application/vnd.apple.mpegurl";

export class HlsVideoEngine implements PlaybackEngine {
  readonly kind = "native" as const;
  readonly performance: PlaybackPerformance | null = null;
  error: Error | null = null;
  private hls: Hls | null = null;

  constructor(readonly element: HTMLVideoElement) {}

  async setSource(source: string, poster?: string | null) {
    const { default: Hls } = await import("hls.js");
    this.error = null;
    this.element.poster = poster ?? "";
    if (Hls.isSupported()) {
      const hls = new Hls({
        enableWorker: true,
        lowLatencyMode: false,
        backBufferLength: 90,
      });
      this.hls = hls;
      await new Promise<void>((resolve, reject) => {
        let settled = false;
        const finish = (cause?: Error) => {
          if (settled) return;
          settled = true;
          hls.off(Hls.Events.MANIFEST_PARSED, onManifest);
          hls.off(Hls.Events.ERROR, onError);
          if (cause) {
            this.error = cause;
            reject(cause);
          } else {
            resolve();
          }
        };
        const onManifest = () => finish();
        const onError = (_event: string, data: { fatal?: boolean; details?: string }) => {
          if (data.fatal) finish(new Error(`HLS 加载失败${data.details ? `：${data.details}` : ""}`));
        };
        hls.on(Hls.Events.MANIFEST_PARSED, onManifest);
        hls.on(Hls.Events.ERROR, onError);
        hls.loadSource(source);
        hls.attachMedia(this.element);
      });
      return;
    }
    if (this.element.canPlayType(HLS_MIME) === "") {
      throw new Error("当前浏览器不支持 HLS");
    }
    this.element.src = source;
    this.element.load();
  }

  play() {
    return this.element.play();
  }

  pause() {
    this.element.pause();
  }

  seek(seconds: number) {
    if (Number.isFinite(seconds) && seconds >= 0) this.element.currentTime = seconds;
  }

  snapshot(): PlaybackSnapshot {
    return {
      currentTime: Number.isFinite(this.element.currentTime) ? Math.max(0, this.element.currentTime) : 0,
      duration: Number.isFinite(this.element.duration) ? Math.max(0, this.element.duration) : null,
      ended: this.element.ended,
    };
  }

  destroy() {
    this.hls?.destroy();
    this.hls = null;
    this.element.pause();
    this.element.removeAttribute("src");
    this.element.removeAttribute("poster");
    this.element.load();
  }
}
