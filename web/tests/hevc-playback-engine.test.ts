import { describe, expect, it, vi } from "vitest";
import { copySegmentBytes, normalizeFragmentCompositionOffsets, segmentSampleCount, segmentSampleCountForTracks } from "../src/features/player/hevc-playback-engine";
import { isHevcCodec } from "../src/features/player/media-codec";
import { SourceBufferQueue, bufferedAheadSeconds, evictableBufferEnd, waitForBufferDrain } from "../src/features/player/mse-buffer-control";

describe("HEVC playback engine helpers", () => {
  it("recognizes HEVC codec labels without confusing Dolby Vision metadata", () => {
    expect(isHevcCodec("hvc1.2.4.L153.B0")).toBe(true);
    expect(isHevcCodec("hev1.1.6.L120.B0")).toBe(true);
    expect(isHevcCodec("HEVC")).toBe(true);
    expect(isHevcCodec("dvh1.05.06")).toBe(false);
    expect(isHevcCodec("avc1.640028")).toBe(false);
  });

  it("chooses a bounded segment sample count from track timing", () => {
    expect(segmentSampleCount({ nb_samples: 288, duration: 288, timescale: 24 })).toBe(48);
    expect(segmentSampleCount({ nb_samples: 565, duration: 578560, timescale: 48000 })).toBe(94);
    expect(segmentSampleCount({ nb_samples: 0, duration: 0, timescale: 0 })).toBe(1);
  });

  it("copies initialization bytes before a worker can transfer the source buffer", () => {
    const original = new Uint8Array([1, 2, 3]);
    const copy = copySegmentBytes(original.buffer);

    original[0] = 9;

    expect([...copy]).toEqual([1, 2, 3]);
    expect(copy.buffer).not.toBe(original.buffer);
  });

  it("uses one MP4Box segment sample count for every configured track", () => {
    expect(segmentSampleCountForTracks({ nb_samples: 288, duration: 288, timescale: 24 })).toEqual({ video: 48, audio: 48 });
  });

  it("marks unsigned negative composition offsets as signed trun values", () => {
    const fragment = new Uint8Array(36);
    const view = new DataView(fragment.buffer);
    view.setUint32(0, 36);
    fragment.set([109, 111, 111, 102], 4);
    view.setUint32(8, 28);
    fragment.set([116, 114, 97, 102], 12);
    view.setUint32(16, 20);
    fragment.set([116, 114, 117, 110], 20);
    view.setUint32(24, 0x00000800);
    view.setUint32(28, 1);
    view.setUint32(32, 0xffffffff);

    const normalized = normalizeFragmentCompositionOffsets(fragment.buffer);

    expect(normalized[24]).toBe(1);
    expect(new DataView(normalized.buffer).getUint32(32)).toBe(0xffffffff);
    expect(normalized.buffer).not.toBe(fragment.buffer);
  });

  it("computes buffered ahead window including slight initial PTS offset and back-buffer eviction", async () => {
    const makeRanges = (ranges: Array<[number, number]>): TimeRanges => ({
      length: ranges.length,
      start: (index: number) => ranges[index]![0],
      end: (index: number) => ranges[index]![1],
    });

    expect(bufferedAheadSeconds({ currentTime: 0, buffered: makeRanges([[0.083, 72]]) })).toBe(72);
    expect(bufferedAheadSeconds({ currentTime: 25, buffered: makeRanges([[0, 72]]) })).toBe(47);
    expect(evictableBufferEnd(makeRanges([[0, 120]]), 50)).toBeNull();
    expect(evictableBufferEnd(makeRanges([[0, 140]]), 100)).toBe(40);
    expect(evictableBufferEnd(makeRanges([[30, 140]]), 100)).toBeNull();

    let currentAheadEnd = 80;
    const listeners = new Map<string, Set<EventListener>>();
    const mockVideo = {
      currentTime: 0,
      get buffered() {
        return makeRanges([[0, currentAheadEnd]]);
      },
      addEventListener: (event: string, listener: EventListener) => {
        if (!listeners.has(event)) listeners.set(event, new Set());
        listeners.get(event)!.add(listener);
      },
      removeEventListener: (event: string, listener: EventListener) => {
        listeners.get(event)?.delete(listener);
      },
    } as unknown as HTMLMediaElement;

    const abort = new AbortController();
    let drained = false;
    const drainPromise = waitForBufferDrain(mockVideo, abort.signal).then(() => {
      drained = true;
    });
    await Promise.resolve();
    expect(drained).toBe(false);

    mockVideo.currentTime = 55;
    listeners.get("timeupdate")?.forEach((cb) => cb(new Event("timeupdate")));
    await drainPromise;
    expect(drained).toBe(true);

    const removed: Array<[number, number]> = [];
    const sbTarget = new EventTarget();
    const mockSourceBuffer = Object.assign(sbTarget, {
      buffered: makeRanges([[0, 130]]),
      appendBuffer: vi.fn(() => {
        queueMicrotask(() => sbTarget.dispatchEvent(new Event("updateend")));
      }),
      remove: vi.fn((start: number, end: number) => {
        removed.push([start, end]);
        queueMicrotask(() => sbTarget.dispatchEvent(new Event("updateend")));
      }),
    }) as unknown as SourceBuffer;

    const queue = new SourceBufferQueue(mockSourceBuffer);
    await queue.append(new Uint8Array([1, 2, 3]));
    await queue.trimBefore(110);
    expect(removed).toEqual([[0, 50]]);
  });
});
