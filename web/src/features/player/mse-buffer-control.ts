export const MSE_MAX_BUFFER_AHEAD_SECONDS = 60;
export const MSE_LOW_WATER_AHEAD_SECONDS = 30;
export const MSE_BACK_BUFFER_RETENTION_SECONDS = 60;
export const MSE_EVICTION_MIN_CURRENT_TIME = 90;
export const MSE_MIN_EVICTION_SPAN_SECONDS = 15;

export function bufferedAheadSeconds(
  element: Pick<HTMLMediaElement, "buffered" | "currentTime">,
): number {
  const buffered = element.buffered;
  if (!buffered || buffered.length === 0) return 0;
  const currentTime = Number.isFinite(element.currentTime)
    ? Math.max(0, element.currentTime)
    : 0;
  for (let index = 0; index < buffered.length; index += 1) {
    const start = buffered.start(index);
    const end = buffered.end(index);
    if (start <= currentTime + 1 && end > currentTime) {
      return Math.max(0, end - currentTime);
    }
  }
  return 0;
}

export function evictableBufferEnd(
  buffered: TimeRanges | null | undefined,
  currentTime: number,
): number | null {
  if (
    !Number.isFinite(currentTime)
    || currentTime < MSE_EVICTION_MIN_CURRENT_TIME
    || !buffered
    || buffered.length === 0
  ) {
    return null;
  }
  const safeEnd = currentTime - MSE_BACK_BUFFER_RETENTION_SECONDS;
  const start = buffered.start(0);
  if (safeEnd - start < MSE_MIN_EVICTION_SPAN_SECONDS) {
    return null;
  }
  return safeEnd;
}

export function waitForBufferDrain(
  element: HTMLMediaElement,
  signal: AbortSignal,
  lowWaterSeconds = MSE_LOW_WATER_AHEAD_SECONDS,
): Promise<void> {
  if (signal.aborted) {
    return Promise.reject(new DOMException("播放已取消", "AbortError"));
  }
  if (bufferedAheadSeconds(element) <= MSE_MAX_BUFFER_AHEAD_SECONDS) {
    return Promise.resolve();
  }
  return new Promise<void>((resolve, reject) => {
    const cleanup = () => {
      clearInterval(timer);
      element.removeEventListener("timeupdate", check);
      element.removeEventListener("seeking", check);
      element.removeEventListener("emptied", onEmptied);
      signal.removeEventListener("abort", onAbort);
    };
    const check = () => {
      if (signal.aborted) {
        cleanup();
        reject(new DOMException("播放已取消", "AbortError"));
        return;
      }
      if (bufferedAheadSeconds(element) <= lowWaterSeconds) {
        cleanup();
        resolve();
      }
    };
    const onEmptied = () => {
      cleanup();
      resolve();
    };
    const onAbort = () => {
      cleanup();
      reject(new DOMException("播放已取消", "AbortError"));
    };
    const timer = setInterval(check, 500);
    element.addEventListener("timeupdate", check);
    element.addEventListener("seeking", check);
    element.addEventListener("emptied", onEmptied, { once: true });
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

export class SourceBufferQueue {
  private chain = Promise.resolve();

  constructor(
    private readonly sourceBuffer: SourceBuffer,
    private readonly errorPrefix = "MSE SourceBuffer",
  ) {}

  append(data: ArrayBuffer | Uint8Array) {
    const next = this.chain.then(() => this.appendNow(data));
    this.chain = next.catch(() => undefined);
    return next;
  }

  trimBefore(currentTime: number) {
    const next = this.chain.then(() => this.trimBeforeNow(currentTime));
    this.chain = next.catch(() => undefined);
    return next;
  }

  private appendNow(data: ArrayBuffer | Uint8Array) {
    return new Promise<void>((resolve, reject) => {
      const cleanup = () => {
        this.sourceBuffer.removeEventListener("updateend", onUpdateEnd);
        this.sourceBuffer.removeEventListener("error", onError);
      };
      const onUpdateEnd = () => {
        cleanup();
        resolve();
      };
      const onError = () => {
        cleanup();
        reject(new Error(`${this.errorPrefix} append failed`));
      };
      this.sourceBuffer.addEventListener("updateend", onUpdateEnd, { once: true });
      this.sourceBuffer.addEventListener("error", onError, { once: true });
      try {
        if (data instanceof Uint8Array) {
          const copy = new Uint8Array(data.byteLength);
          copy.set(data);
          this.sourceBuffer.appendBuffer(copy.buffer);
        } else {
          this.sourceBuffer.appendBuffer(data);
        }
      } catch (error) {
        cleanup();
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    });
  }

  private trimBeforeNow(currentTime: number) {
    if (typeof this.sourceBuffer.remove !== "function") {
      return Promise.resolve();
    }
    const removeEnd = evictableBufferEnd(this.sourceBuffer.buffered, currentTime);
    if (removeEnd === null) {
      return Promise.resolve();
    }
    return new Promise<void>((resolve) => {
      const cleanup = () => {
        this.sourceBuffer.removeEventListener("updateend", onDone);
        this.sourceBuffer.removeEventListener("error", onDone);
      };
      const onDone = () => {
        cleanup();
        resolve();
      };
      this.sourceBuffer.addEventListener("updateend", onDone, { once: true });
      this.sourceBuffer.addEventListener("error", onDone, { once: true });
      try {
        this.sourceBuffer.remove(0, removeEnd);
      } catch {
        cleanup();
        resolve();
      }
    });
  }
}
