import type { ComponentPropsWithRef, ReactNode, Ref } from "react";
import { cx } from "./cx";

export type LuxDisclosureProps = Omit<ComponentPropsWithRef<"details">, "children"> & {
  summary: ReactNode;
  children: ReactNode;
  /** 收起时也显示的辅助说明。 */
  hint?: ReactNode;
};

/** 折叠面板：原生 details/summary 语义 + Lux 摘要栏。 */
export function LuxDisclosure({ summary, children, hint, className, ref, ...rest }: LuxDisclosureProps) {
  return (
    <details {...rest} ref={ref as Ref<HTMLDetailsElement>} className={cx("lux-disclosure", className)} data-lux-control="disclosure">
      <summary className="lux-disclosure-summary">
        <span className="lux-disclosure-label">{summary}</span>
        {hint === undefined ? null : <span className="lux-disclosure-hint">{hint}</span>}
        <span className="lux-disclosure-chevron" aria-hidden="true" />
      </summary>
      <div className="lux-disclosure-body">{children}</div>
    </details>
  );
}
