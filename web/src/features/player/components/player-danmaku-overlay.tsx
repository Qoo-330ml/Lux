import { useCallback, useEffect, useRef, useState } from "react";
import type { MeasureText } from "../danmaku/measure";
import { createDanmakuEngine, type DanmakuEngine } from "../danmaku/engine";
import { createDanmakuScheduler, type DanmakuScheduler } from "../danmaku/schedule";
import { useDanmakuTrack } from "../danmaku/useDanmakuTrack";

const DEFAULT_VIEWPORT = { width: 1280, height: 720 };

type PlayerDanmakuOverlayProps = {
  itemId: string;
  sourceId: string;
  visible: boolean;
  currentTime: number;
  playbackRate: number;
  lifecycleKey: string;
  /**
   * Optional. When the video is playing, the renderer interpolates the media clock between the
   * sparse `timeupdate` samples with this flag and `playbackRate`; paused danmaku stay still.
   */
  playing?: boolean;
  /** Optional. Receives "弹幕加载失败" / "已截断" / "内容为空" style messages, `null` when fine. */
  onStatusChange?: (status: string | null) => void;
  /** Optional. Injects text measurement (tests, or a font probe). */
  measure?: MeasureText;
};

/**
 * Danmaku overlay.
 *
 * React owns only the host element and reloads data per playback lifecycle; the danmaku nodes and
 * their per-frame `transform` updates live in `danmaku/engine.ts` so a 4-times-per-second
 * `timeupdate` no longer re-renders the component tree.
 */
export function PlayerDanmakuOverlay({
  itemId,
  sourceId,
  visible,
  currentTime,
  playbackRate,
  lifecycleKey,
  playing = false,
  onStatusChange,
  measure,
}: PlayerDanmakuOverlayProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const engineRef = useRef<DanmakuEngine | null>(null);
  const schedulerRef = useRef<DanmakuScheduler | null>(null);
  const clockRef = useRef({ mediaTime: currentTime, playing, playbackRate });
  const measureRef = useRef(measure);
  const [viewport, setViewport] = useState(DEFAULT_VIEWPORT);
  const track = useDanmakuTrack({ itemId, sourceId, visible, lifecycleKey, onStatusChange });

  const syncClock = useCallback(() => {
    engineRef.current?.sync(clockRef.current);
  }, []);

  // Refresh the clock/measurer refs before the effects below use them in this same commit.
  useEffect(() => {
    clockRef.current = { mediaTime: currentTime, playing, playbackRate };
    measureRef.current = measure;
  });

  // Track the host size so lane geometry matches the visible overlay box.
  useEffect(() => {
    if (!visible) return;
    const host = hostRef.current;
    if (!host) return;
    const updateViewport = () => {
      const width = Math.max(1, host.clientWidth || DEFAULT_VIEWPORT.width);
      const height = Math.max(1, host.clientHeight || DEFAULT_VIEWPORT.height);
      setViewport((previous) => (
        previous.width === width && previous.height === height ? previous : { width, height }
      ));
    };
    updateViewport();
    if (typeof ResizeObserver !== "undefined") {
      const observer = new ResizeObserver(updateViewport);
      observer.observe(host);
      return () => observer.disconnect();
    }
    window.addEventListener("resize", updateViewport);
    return () => window.removeEventListener("resize", updateViewport);
  }, [visible]);

  // A fresh schedule per data/viewport change: lanes are assigned from the current window.
  useEffect(() => {
    schedulerRef.current = createDanmakuScheduler(track.entries, {
      viewport,
      measure: measureRef.current,
    });
    syncClock();
  }, [measure, syncClock, track.entries, viewport]);

  // The engine binds to the host element; it is the only owner of the danmaku DOM nodes.
  useEffect(() => {
    if (!visible) return;
    const host = hostRef.current;
    if (!host) return;
    const engine = createDanmakuEngine({
      container: host,
      scheduler: () => schedulerRef.current,
    });
    engineRef.current = engine;
    engine.sync(clockRef.current);
    return () => {
      engine.destroy();
      engineRef.current = null;
    };
  }, [visible]);

  // Every React sample re-anchors the clock; animation frames interpolate in between.
  useEffect(() => {
    syncClock();
  }, [currentTime, playbackRate, playing, syncClock]);

  if (!visible) return null;
  return (
    <div
      ref={hostRef}
      className="lux-player-danmaku-overlay"
      data-lux-danmaku-overlay
      aria-hidden="true"
    />
  );
}
