import type { ReactNode } from "react";
import { cx } from "./cx";

export type LuxProgressProps = {
  value: number;
  max?: number;
  label: string;
  /** 视觉上的高度层级。 */
  size?: "default" | "compact";
  className?: string;
  hint?: ReactNode;
  indeterminate?: boolean;
};

/** 进度条：role="progressbar" + Lux 轨道/填充。 */
export function LuxProgress({ value, max = 100, label, size = "default", className, hint, indeterminate = false }: LuxProgressProps) {
  const clampedMax = max > 0 ? max : 100;
  const clamped = Math.min(Math.max(value, 0), clampedMax);
  const percent = indeterminate ? 0 : Math.round((clamped / clampedMax) * 1000) / 10;

  return (
    <div className={cx("lux-progress-bar", size === "compact" && "lux-progress-compact", indeterminate && "is-indeterminate", className)}>
      <div
        className="lux-progress-track"
        role="progressbar"
        aria-label={label}
        aria-valuemin={indeterminate ? undefined : 0}
        aria-valuemax={indeterminate ? undefined : clampedMax}
        aria-valuenow={indeterminate ? undefined : clamped}
        aria-valuetext={indeterminate ? undefined : `${percent}%`}
      >
        <span className="lux-progress-fill" style={indeterminate ? undefined : { width: `${percent}%` }} />
      </div>
      {hint === undefined ? null : <span className="lux-progress-hint">{hint}</span>}
    </div>
  );
}
