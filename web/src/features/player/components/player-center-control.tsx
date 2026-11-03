import { useEffect, useId, useRef, useState } from "react";

type Point = readonly [number, number];
/** Each facet is a 5-point polygon so two shapes interpolate point by point. */
export type MarkShape = readonly (readonly Point[])[];

/**
 * The Lux mark: top facet, left pillar and the play-arrow facet (same geometry as /logo-white.svg).
 */
export const LUX_MARK_SHAPE: MarkShape = [
  [[175, 25], [435, 165], [325, 235], [305, 225], [65, 85]],
  [[55, 95], [165, 165], [165, 475], [55, 385], [55, 385]],
  [[445, 185], [435, 325], [185, 475], [185, 345], [185, 345]],
];

/** The same three facets folded into two vertical bars (top and left facets form the left bar). */
export const PAUSE_BARS_SHAPE: MarkShape = [
  [[95, 40], [190, 40], [190, 460], [95, 460], [95, 460]],
  [[95, 40], [190, 40], [190, 460], [95, 460], [95, 460]],
  [[310, 40], [405, 40], [405, 460], [310, 460], [310, 460]],
];

const MORPH_MS = 480;

export function interpolateShape(from: MarkShape, to: MarkShape, progress: number): MarkShape {
  return from.map((facet, facetIndex) => facet.map((point, pointIndex): Point => {
    const target = to[facetIndex]?.[pointIndex] ?? point;
    return [
      point[0] + (target[0] - point[0]) * progress,
      point[1] + (target[1] - point[1]) * progress,
    ];
  }));
}

export function shapeToPath(facet: readonly Point[]): string {
  return `M${facet.map(([x, y]) => `${x.toFixed(1)} ${y.toFixed(1)}`).join(" L")} Z`;
}

const easeInOutCubic = (value: number) => (value < 0.5 ? 4 * value ** 3 : 1 - (-2 * value + 2) ** 3 / 2);

function prefersReducedMotion(): boolean {
  return typeof window !== "undefined"
    && typeof window.matchMedia === "function"
    && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/** Eases from the current shape to `target` whenever the target changes. */
function useMorphingShape(target: MarkShape): MarkShape {
  const [shape, setShape] = useState<MarkShape>(target);
  const shapeRef = useRef<MarkShape>(target);
  useEffect(() => {
    if (shapeRef.current === target) return undefined;
    const from = shapeRef.current;
    if (prefersReducedMotion()) {
      shapeRef.current = target;
      setShape(target);
      return undefined;
    }
    let frame = 0;
    let start: number | null = null;
    const step = (now: number) => {
      if (start === null) start = now;
      const progress = Math.min(1, (now - start) / MORPH_MS);
      shapeRef.current = progress >= 1 ? target : interpolateShape(from, target, easeInOutCubic(progress));
      setShape(shapeRef.current);
      if (progress < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  }, [target]);
  return shape;
}

type PlayerCenterControlProps = {
  playing: boolean;
  /** Shows the mark breathing while the player prepares a stream. */
  loading?: boolean;
  onClick: () => void;
};

/**
 * Centre play/pause control: the Lux mark while paused, folding into two bars while playing.
 */
export function PlayerCenterControl({ playing, loading = false, onClick }: PlayerCenterControlProps) {
  const showBars = playing && !loading;
  const shape = useMorphingShape(showBars ? PAUSE_BARS_SHAPE : LUX_MARK_SHAPE);
  const gradientId = `lux-center-${useId().replace(/[^a-zA-Z0-9]/g, "")}`;
  const label = loading ? "加载中" : playing ? "暂停" : "播放";

  return (
    <button
      type="button"
      className={`lux-player-center-control${playing ? " is-playing" : ""}${loading ? " is-loading" : ""}`}
      aria-label={label}
      title={`${label} (空格)`}
      onClick={onClick}
    >
      <svg viewBox="0 0 500 500" aria-hidden="true" focusable="false">
        <defs>
          <linearGradient id={`${gradientId}-top`} x1="175" y1="25" x2="385" y2="205" gradientUnits="userSpaceOnUse">
            <stop offset="0%" stopColor="#FFFFFF" />
            <stop offset="60%" stopColor="#EEF2F8" />
            <stop offset="100%" stopColor="#DCE2EC" />
          </linearGradient>
          <linearGradient id={`${gradientId}-left`} x1="110" y1="95" x2="110" y2="475" gradientUnits="userSpaceOnUse">
            <stop offset="0%" stopColor="#F8FAFC" />
            <stop offset="50%" stopColor="#E2E8F0" />
            <stop offset="100%" stopColor="#CBD5E1" />
          </linearGradient>
          <linearGradient id={`${gradientId}-bottom`} x1="185" y1="425" x2="445" y2="225" gradientUnits="userSpaceOnUse">
            <stop offset="0%" stopColor="#CBD5E1" />
            <stop offset="60%" stopColor="#E2E8F0" />
            <stop offset="100%" stopColor="#F8FAFC" />
          </linearGradient>
        </defs>
        <path d={shapeToPath(shape[0] ?? [])} fill={`url(#${gradientId}-top)`} />
        <path d={shapeToPath(shape[1] ?? [])} fill={`url(#${gradientId}-left)`} />
        <path d={shapeToPath(shape[2] ?? [])} fill={`url(#${gradientId}-bottom)`} />
      </svg>
    </button>
  );
}
