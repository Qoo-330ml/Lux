import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { MeasureText } from "../danmaku/measure";
import { createDanmakuEngine, type DanmakuEngine } from "../danmaku/engine";
import { createDanmakuScheduler, type DanmakuScheduler } from "../danmaku/schedule";
import {
  DEFAULT_DANMAKU_SETTINGS,
  danmakuLayoutFromSettings,
  danmakuLayoutsEqual,
  type DanmakuLayoutOptions,
  type DanmakuSettings,
} from "../danmaku/settings";
import { useDanmakuTrack } from "../danmaku/useDanmakuTrack";

const DEFAULT_VIEWPORT = { width: 1280, height: 720 };

function requestLayoutFrame(callback: () => void): number {
  if (typeof requestAnimationFrame === "function") return requestAnimationFrame(() => callback());
  return setTimeout(callback, 0) as unknown as number;
}

function cancelLayoutFrame(handle: number) {
  if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(handle);
  else clearTimeout(handle);
}

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
  /**
   * Optional browser-local danmaku settings. They are applied live: opacity on the container, the
   * rest through the scheduler's `updateLayout`, so no XML is fetched or parsed again.
   */
  settings?: DanmakuSettings;
};

/**
 * Danmaku overlay.
 *
 * React owns only the host element and reloads data per playback lifecycle; the danmaku nodes and
 * their per-frame `transform` updates live in `danmaku/engine.ts` so a 4-times-per-second
 * `timeupdate` no longer re-renders the component tree. Settings changes reach the scheduler and the
 * existing nodes in place.
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
  settings = DEFAULT_DANMAKU_SETTINGS,
}: PlayerDanmakuOverlayProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const engineRef = useRef<DanmakuEngine | null>(null);
  const schedulerRef = useRef<DanmakuScheduler | null>(null);
  const clockRef = useRef({ mediaTime: currentTime, playing, playbackRate });
  const measureRef = useRef(measure);
  const [viewport, setViewport] = useState(DEFAULT_VIEWPORT);
  const layout = useMemo<DanmakuLayoutOptions>(
    () => danmakuLayoutFromSettings(settings),
    [settings],
  );
  // Read by the scheduler-creation effect so a layout change never has to recreate it just to keep
  // the current options.
  const layoutRef = useRef<DanmakuLayoutOptions>(layout);
  // Layout updates are coalesced into one animation frame: dragging a slider fires an input event per
  // step, and each one must not walk a 50k-entry danmaku file.
  const layoutFrameRef = useRef<number | null>(null);
  const appliedLayoutRef = useRef<DanmakuLayoutOptions | null>(null);
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
      layout: layoutRef.current,
    });
    appliedLayoutRef.current = layoutRef.current;
    syncClock();
  }, [measure, syncClock, track.entries, viewport]);

  // Settings changes are coalesced into one frame, then re-lane the current window and redraw;
  // existing nodes keep their identity, so a paused video updates without any reload.
  useEffect(() => {
    layoutRef.current = layout;
    const scheduler = schedulerRef.current;
    if (!scheduler) return;
    const applied = appliedLayoutRef.current;
    if (applied && danmakuLayoutsEqual(applied, layout)) return;
    if (layoutFrameRef.current !== null) return;
    layoutFrameRef.current = requestLayoutFrame(() => {
      layoutFrameRef.current = null;
      const active = schedulerRef.current;
      if (!active) return;
      active.updateLayout(layoutRef.current);
      appliedLayoutRef.current = layoutRef.current;
      syncClock();
    });
    return () => {
      if (layoutFrameRef.current === null) return;
      cancelLayoutFrame(layoutFrameRef.current);
      layoutFrameRef.current = null;
    };
  }, [layout, syncClock]);

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
      style={{ opacity: settings.opacity }}
      aria-hidden="true"
    />
  );
}
