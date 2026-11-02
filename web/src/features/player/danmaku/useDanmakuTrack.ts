/**
 * Danmaku track data hook.
 *
 * Owns one load per (item, source, lifecycle) generation: aborts superseded loads, treats HTTP 404 as
 * a remembered empty state (requested once, never retried), and reports a user-facing status through
 * the optional `onStatusChange` callback.
 */

import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../../lib/api/client";
import type { LuxDanmakuEntry } from "./parse";
import {
  danmakuFailureMessage,
  danmakuSourceKey,
  danmakuStatusMessage,
  isKnownEmptyDanmakuSource,
  loadDanmakuDocument,
  rememberEmptyDanmakuSource,
} from "./load";

export type DanmakuTrack = {
  entries: LuxDanmakuEntry[];
  truncated: boolean;
  totalEntries: number;
};

export type UseDanmakuTrackOptions = {
  itemId: string;
  sourceId: string;
  visible: boolean;
  lifecycleKey: string;
  onStatusChange?: (status: string | null) => void;
};

const EMPTY_TRACK: DanmakuTrack = { entries: [], truncated: false, totalEntries: 0 };

export function useDanmakuTrack({
  itemId,
  sourceId,
  visible,
  lifecycleKey,
  onStatusChange,
}: UseDanmakuTrackOptions): DanmakuTrack {
  const [track, setTrack] = useState<DanmakuTrack>(EMPTY_TRACK);
  const generationRef = useRef(0);
  // The status callback is read through a ref so an inline arrow from the page cannot restart the
  // load effect on every render.
  const statusRef = useRef(onStatusChange);

  useEffect(() => {
    statusRef.current = onStatusChange;
  }, [onStatusChange]);

  useEffect(() => {
    const generation = ++generationRef.current;
    const controller = new AbortController();
    setTrack(EMPTY_TRACK);
    statusRef.current?.(null);
    if (!visible || !itemId || !sourceId) return () => controller.abort();
    const sourceKey = danmakuSourceKey(itemId, sourceId);
    // A known-empty source stays empty: no second request, no error state, no 404 in the console.
    if (isKnownEmptyDanmakuSource(sourceKey)) return () => controller.abort();

    const isCurrent = () => generationRef.current === generation && !controller.signal.aborted;

    const load = async () => {
      try {
        const result = await loadDanmakuDocument({ itemId, sourceId, signal: controller.signal });
        if (!isCurrent()) return;
        setTrack({
          entries: result.entries,
          truncated: result.truncated,
          totalEntries: result.totalEntries,
        });
        statusRef.current?.(danmakuStatusMessage(result));
      } catch (error) {
        if (controller.signal.aborted) return;
        // 404 means “this source has no danmaku sidecar”: a normal empty result, not a failure.
        if (error instanceof ApiError && error.status === 404) {
          rememberEmptyDanmakuSource(sourceKey);
          return;
        }
        if (!isCurrent()) return;
        setTrack(EMPTY_TRACK);
        statusRef.current?.(danmakuFailureMessage(error));
      }
    };
    void load();
    return () => controller.abort();
  }, [itemId, lifecycleKey, sourceId, visible]);

  return track;
}
