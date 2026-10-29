import { useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import { timelinePosition } from "./player-page-helpers";

type ProgressBarRef = { current: HTMLDivElement | null };

export type PlayerTimelineScrubber = {
  hoverTime: number | null;
  hoverPercent: number | null;
  isDraggingScrubberRef: { current: boolean };
  handleScrubberPointerDown: (e: ReactPointerEvent<HTMLDivElement>) => void;
  handleScrubberPointerMove: (event: ReactPointerEvent<HTMLDivElement>) => void;
  finishScrubberPointer: (event: ReactPointerEvent<HTMLDivElement>, commitPosition: boolean) => void;
  handleScrubberMouseMove: (e: ReactMouseEvent<HTMLDivElement>) => void;
  handleScrubberMouseLeave: () => void;
  handleTimelineKeyDown: (event: ReactKeyboardEvent<HTMLDivElement>) => void;
};

type PlayerTimelineScrubberOptions = {
  progressBarRef: ProgressBarRef;
  duration: number;
  seekTo: (seconds: number) => void;
  seekRelative: (seconds: number) => void;
  setIsPointerInteracting: (value: boolean) => void;
  resetControlsTimeout: () => void;
};

/**
 * Pointer, hover and keyboard interaction for the progress bar. Dragging keeps a
 * pointer id so a second pointer cannot hijack an in-flight scrub, and capture is
 * released both on commit and on cancel.
 */
export function usePlayerTimelineScrubber({
  progressBarRef,
  duration,
  seekTo,
  seekRelative,
  setIsPointerInteracting,
  resetControlsTimeout,
}: PlayerTimelineScrubberOptions): PlayerTimelineScrubber {
  const [hoverTime, setHoverTime] = useState<number | null>(null);
  const [hoverPercent, setHoverPercent] = useState<number | null>(null);
  const isDraggingScrubberRef = useRef(false);
  const scrubberPointerIdRef = useRef<number | null>(null);

  const handleScrubberPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    const bar = progressBarRef.current;
    if (
      !bar
      || !duration
      || scrubberPointerIdRef.current !== null
      || (e.pointerType === "mouse" && e.button !== 0)
    ) return;

    isDraggingScrubberRef.current = true;
    scrubberPointerIdRef.current = e.pointerId;
    setIsPointerInteracting(true);
    resetControlsTimeout();
    try {
      bar.setPointerCapture?.(e.pointerId);
    } catch {
      // A cancelled pointer can no longer be captured; the timeline still
      // receives its element-scoped fallback events.
    }
    seekTo(timelinePosition(bar, e.clientX, duration));
  };

  const handleScrubberPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const bar = progressBarRef.current;
    if (!bar || scrubberPointerIdRef.current !== event.pointerId) return;
    seekTo(timelinePosition(bar, event.clientX, duration));
  };

  const finishScrubberPointer = (
    event: ReactPointerEvent<HTMLDivElement>,
    commitPosition: boolean,
  ) => {
    const bar = progressBarRef.current;
    if (!bar || scrubberPointerIdRef.current !== event.pointerId) return;
    try {
      if (event.currentTarget.hasPointerCapture?.(event.pointerId)) {
        event.currentTarget.releasePointerCapture?.(event.pointerId);
      }
    } catch {
      // Browsers may release capture before a pointercancel reaches React.
    }
    isDraggingScrubberRef.current = false;
    scrubberPointerIdRef.current = null;
    setIsPointerInteracting(false);
    if (commitPosition) seekTo(timelinePosition(bar, event.clientX, duration));
    resetControlsTimeout();
  };

  const handleScrubberMouseMove = (e: ReactMouseEvent<HTMLDivElement>) => {
    const bar = progressBarRef.current;
    if (!bar || !duration) return;
    const rect = bar.getBoundingClientRect();
    const pos = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    setHoverPercent(pos * 100);
    setHoverTime(pos * duration);
  };

  const handleScrubberMouseLeave = () => {
    setHoverTime(null);
    setHoverPercent(null);
  };

  const handleTimelineKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      seekRelative(-5);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      seekRelative(5);
    } else if (event.key === "Home") {
      event.preventDefault();
      seekTo(0);
    } else if (event.key === "End") {
      event.preventDefault();
      seekTo(duration);
    }
  };

  return {
    hoverTime,
    hoverPercent,
    isDraggingScrubberRef,
    handleScrubberPointerDown,
    handleScrubberPointerMove,
    finishScrubberPointer,
    handleScrubberMouseMove,
    handleScrubberMouseLeave,
    handleTimelineKeyDown,
  };
}
