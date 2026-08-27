import { useCallback, useEffect, useRef } from "react";
import type { MouseEvent as ReactMouseEvent, PointerEvent as ReactPointerEvent } from "react";

export type PlayerGestureInput = {
  startX: number;
  startY: number;
  currentX: number;
  currentY: number;
  width: number;
  height: number;
  currentTime: number;
  duration: number;
  volume: number;
};

export type PlayerGesture =
  | { type: "SEEK"; position: number }
  | { type: "VOLUME"; volume: number };

const MIN_SWIPE_DISTANCE_PX = 18;
const DOUBLE_TAP_DELAY_MS = 280;
const DOUBLE_TAP_DISTANCE_PX = 48;
const CLICK_SUPPRESSION_DELAY_MS = 420;

export function resolvePlayerGesture(input: PlayerGestureInput): PlayerGesture | null {
  if (
    !Number.isFinite(input.width) || input.width <= 0
    || !Number.isFinite(input.height) || input.height <= 0
  ) {
    return null;
  }

  const deltaX = input.currentX - input.startX;
  const deltaY = input.currentY - input.startY;
  if (Math.max(Math.abs(deltaX), Math.abs(deltaY)) < MIN_SWIPE_DISTANCE_PX) return null;

  if (Math.abs(deltaX) >= Math.abs(deltaY)) {
    if (!Number.isFinite(input.duration) || input.duration <= 0) return null;
    return {
      type: "SEEK",
      position: clamp(input.currentTime + (deltaX / input.width) * input.duration, 0, input.duration),
    };
  }

  return {
    type: "VOLUME",
    volume: clamp(input.volume - deltaY / input.height, 0, 1),
  };
}

function clamp(value: number, minimum: number, maximum: number) {
  return Math.min(maximum, Math.max(minimum, value));
}

function capturePointer(target: HTMLVideoElement, pointerId: number) {
  try {
    target.setPointerCapture?.(pointerId);
  } catch {
    // A cancelled or synthetic pointer may no longer be capturable. Its
    // element-scoped handlers still provide a safe fallback for this gesture.
  }
}

function releasePointer(target: HTMLVideoElement, pointerId: number) {
  try {
    if (target.hasPointerCapture?.(pointerId)) target.releasePointerCapture?.(pointerId);
  } catch {
    // The browser can release capture before pointercancel reaches React.
  }
}

type PlayerSurfaceGestureOptions = {
  enabled: boolean;
  currentTime: number;
  duration: number;
  volume: number;
  onSeekTo: (position: number) => void;
  onVolumeChange: (volume: number) => void;
  onSeekRelative: (seconds: number) => void;
  onSingleTap: () => void;
  onActivity: () => void;
  onInteractionChange: (interacting: boolean) => void;
};

type GestureSession = PlayerGestureInput & {
  pointerId: number;
  left: number;
  moved: boolean;
};

type Tap = {
  timestamp: number;
  x: number;
};

/**
 * Lux-owned pointer interaction. It is intentionally scoped to one video
 * element and forwards actions to the caller instead of accessing engines or
 * playback sessions.
 */
export function usePlayerSurfaceGestures(options: PlayerSurfaceGestureOptions) {
  const optionsRef = useRef(options);
  const sessionRef = useRef<GestureSession | null>(null);
  const tapRef = useRef<Tap | null>(null);
  const tapTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const suppressClickUntilRef = useRef(0);
  optionsRef.current = options;

  const clearTapTimer = useCallback(() => {
    if (tapTimerRef.current) clearTimeout(tapTimerRef.current);
    tapTimerRef.current = null;
  }, []);

  useEffect(() => () => clearTapTimer(), [clearTapTimer]);

  const onPointerDown = useCallback((event: ReactPointerEvent<HTMLVideoElement>) => {
    const current = optionsRef.current;
    if (!current.enabled || event.pointerType === "mouse" || sessionRef.current) return;
    const rect = event.currentTarget.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    capturePointer(event.currentTarget, event.pointerId);
    sessionRef.current = {
      pointerId: event.pointerId,
      left: rect.left,
      startX: event.clientX,
      startY: event.clientY,
      currentX: event.clientX,
      currentY: event.clientY,
      width: rect.width,
      height: rect.height,
      currentTime: current.currentTime,
      duration: current.duration,
      volume: current.volume,
      moved: false,
    };
    current.onInteractionChange(true);
    current.onActivity();
  }, []);

  const onPointerMove = useCallback((event: ReactPointerEvent<HTMLVideoElement>) => {
    const session = sessionRef.current;
    if (!session || session.pointerId !== event.pointerId) return;
    const gesture = resolvePlayerGesture({
      ...session,
      currentX: event.clientX,
      currentY: event.clientY,
    });
    if (!gesture) return;
    session.moved = true;
    const current = optionsRef.current;
    current.onInteractionChange(true);
    current.onActivity();
    if (event.cancelable) event.preventDefault();
    if (gesture.type === "SEEK") current.onSeekTo(gesture.position);
    else current.onVolumeChange(gesture.volume);
  }, []);

  const finishPointer = useCallback((event: ReactPointerEvent<HTMLVideoElement>, cancelled: boolean) => {
    const session = sessionRef.current;
    if (!session || session.pointerId !== event.pointerId) return;
    sessionRef.current = null;
    releasePointer(event.currentTarget, event.pointerId);
    const current = optionsRef.current;
    current.onInteractionChange(false);
    current.onActivity();
    suppressClickUntilRef.current = Date.now() + CLICK_SUPPRESSION_DELAY_MS;
    if (cancelled || session.moved) return;

    const previousTap = tapRef.current;
    const now = Date.now();
    if (
      previousTap
      && now - previousTap.timestamp <= DOUBLE_TAP_DELAY_MS
      && Math.abs(event.clientX - previousTap.x) <= DOUBLE_TAP_DISTANCE_PX
    ) {
      clearTapTimer();
      tapRef.current = null;
      current.onSeekRelative(event.clientX - session.left < session.width / 2 ? -10 : 10);
      return;
    }

    clearTapTimer();
    tapRef.current = { timestamp: now, x: event.clientX };
    tapTimerRef.current = setTimeout(() => {
      tapRef.current = null;
      optionsRef.current.onSingleTap();
    }, DOUBLE_TAP_DELAY_MS);
  }, [clearTapTimer]);

  const consumeSuppressedClick = useCallback((event: ReactMouseEvent<HTMLVideoElement>) => {
    if (Date.now() >= suppressClickUntilRef.current) return false;
    event.preventDefault();
    event.stopPropagation();
    return true;
  }, []);

  return {
    onPointerDown,
    onPointerMove,
    onPointerUp: (event: ReactPointerEvent<HTMLVideoElement>) => finishPointer(event, false),
    onPointerCancel: (event: ReactPointerEvent<HTMLVideoElement>) => finishPointer(event, true),
    consumeSuppressedClick,
  };
}
