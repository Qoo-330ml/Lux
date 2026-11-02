/**
 * React state for the browser-local danmaku settings.
 *
 * The stored copy is the source of truth on mount; updates apply to the component immediately and are
 * written to `localStorage` at most once per animation frame (the last value wins). Dragging a slider
 * therefore never writes on every input event, while a released slider (`change`), a blurred control,
 * or a closing tab (`pagehide`/unmount) flushes the pending value right away.
 *
 * Storage writes never happen inside a React state updater: StrictMode may invoke an updater twice and
 * a discarded concurrent render may replay it, which would duplicate a write or persist a value that
 * never reached the screen. A ref mirrors the latest committed value so a rapid sequence of updates
 * still composes.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import {
  DEFAULT_DANMAKU_SETTINGS,
  normalizeDanmakuSettings,
  readDanmakuSettings,
  saveDanmakuSettings,
  type DanmakuSettings,
} from "./settings";

export type UseDanmakuSettingsOptions = {
  /** Injected in tests; defaults to this browser's `localStorage`. */
  storage?: Storage | null;
};

export type DanmakuSettingsStore = {
  settings: DanmakuSettings;
  /** Applies a patch immediately to the UI; the storage write is coalesced into one frame. */
  updateSettings(patch: Partial<DanmakuSettings>): void;
  /** Restores the defaults immediately; the storage write is coalesced into one frame. */
  resetSettings(): void;
};

function requestSaveFrame(callback: () => void): number {
  if (typeof requestAnimationFrame === "function") return requestAnimationFrame(() => callback());
  return setTimeout(callback, 0) as unknown as number;
}

function cancelSaveFrame(handle: number) {
  if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(handle);
  else clearTimeout(handle);
}

export function useDanmakuSettings(
  options: UseDanmakuSettingsOptions = {},
): DanmakuSettingsStore {
  const { storage } = options;
  const [settings, setSettings] = useState<DanmakuSettings>(() => readDanmakuSettings(storage));
  // Latest committed value. Only this hook changes `settings`, so writing the ref at commit time
  // (not during render) stays in sync and keeps consecutive updates from dropping each other.
  const settingsRef = useRef(settings);
  const pendingRef = useRef<DanmakuSettings | null>(null);
  const frameRef = useRef<number | null>(null);

  const flushPending = useCallback(() => {
    if (frameRef.current !== null) {
      cancelSaveFrame(frameRef.current);
      frameRef.current = null;
    }
    const pending = pendingRef.current;
    if (pending === null) return;
    pendingRef.current = null;
    saveDanmakuSettings(pending, storage);
  }, [storage]);

  const scheduleSave = useCallback((next: DanmakuSettings) => {
    pendingRef.current = next;
    // A frame is already queued: it will pick up the newest value, so no second write is scheduled.
    if (frameRef.current !== null) return;
    frameRef.current = requestSaveFrame(() => {
      frameRef.current = null;
      flushPending();
    });
  }, [flushPending]);

  // Flush on the events that mean "this interaction is over", and when the hook unmounts. The capture
  // phase catches `change`/`blur` from the slider before they bubble anywhere else.
  useEffect(() => {
    if (typeof window === "undefined") return;
    const flush = () => flushPending();
    window.addEventListener("change", flush, true);
    window.addEventListener("blur", flush, true);
    window.addEventListener("pagehide", flush);
    return () => {
      window.removeEventListener("change", flush, true);
      window.removeEventListener("blur", flush, true);
      window.removeEventListener("pagehide", flush);
      flushPending();
    };
  }, [flushPending]);

  const updateSettings = useCallback((patch: Partial<DanmakuSettings>) => {
    const next = normalizeDanmakuSettings({ ...settingsRef.current, ...patch });
    settingsRef.current = next;
    setSettings(next);
    scheduleSave(next);
  }, [scheduleSave]);

  const resetSettings = useCallback(() => {
    const next = normalizeDanmakuSettings(DEFAULT_DANMAKU_SETTINGS);
    settingsRef.current = next;
    setSettings(next);
    scheduleSave(next);
  }, [scheduleSave]);

  return { settings, updateSettings, resetSettings };
}
