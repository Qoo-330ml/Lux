import type { ComponentPropsWithRef, ReactNode, Ref } from "react";
import { cx } from "./cx";

export type LuxButtonTone = "primary" | "secondary" | "glass" | "danger";
export type LuxButtonSize = "default" | "large" | "compact" | "touch";

export type LuxButtonProps = ComponentPropsWithRef<"button"> & {
  tone?: LuxButtonTone;
  size?: LuxButtonSize;
  block?: boolean;
  icon?: ReactNode;
};

/** 生成 Lux 按钮类名，供 react-router Link 或 label 等非 button 元素复用。 */
export function luxButtonClass(options: { tone?: LuxButtonTone; size?: LuxButtonSize; block?: boolean; className?: string } = {}): string {
  return cx(
    "lux-button",
    options.tone && `lux-button-${options.tone}`,
    options.size && options.size !== "default" && `lux-button-${options.size}`,
    options.block && "lux-button-block",
    options.className,
  );
}

/**
 * Lux 主操作按钮。渲染原生 button，视觉由 .lux-button* 系列提供，
 * 页面不再手写 "lux-button lux-button-secondary" 这类组合类名。
 */
export function LuxButton({ tone, size = "default", block = false, icon, className, children, ref, ...rest }: LuxButtonProps) {
  return (
    <button
      {...rest}
      ref={ref as Ref<HTMLButtonElement>}
      className={luxButtonClass({ tone, size, block, className })}
      data-lux-control="button"
    >
      {icon ? <span className="lux-button-icon" aria-hidden="true">{icon}</span> : null}
      {children}
    </button>
  );
}

export type LuxIconButtonProps = ComponentPropsWithRef<"button"> & {
  size?: "default" | "small";
  danger?: boolean;
  /** 可访问名称，图标按钮必须提供。 */
  label?: string;
};

/** 圆形图标按钮；label 同时写入 aria-label 与 title。 */
export function LuxIconButton({ size = "default", danger = false, label, className, children, ref, title, ...rest }: LuxIconButtonProps) {
  return (
    <button
      {...rest}
      ref={ref as Ref<HTMLButtonElement>}
      aria-label={rest["aria-label"] ?? label}
      title={title ?? label}
      className={cx(
        "lux-icon-button",
        size === "small" && "lux-icon-button-small",
        danger && "lux-danger-icon",
        className,
      )}
      data-lux-control="icon-button"
    >
      {children}
    </button>
  );
}

export type LuxLinkButtonProps = ComponentPropsWithRef<"a"> & {
  tone?: LuxButtonTone;
  size?: LuxButtonSize;
  block?: boolean;
};

/** 链接型动作：用于导航，但沿用 Lux 按钮外观。 */
export function LuxLinkButton({ tone, size = "default", block = false, className, children, ref, ...rest }: LuxLinkButtonProps) {
  return (
    <a
      {...rest}
      ref={ref as Ref<HTMLAnchorElement>}
      className={luxButtonClass({ tone, size, block, className })}
      data-lux-control="link-button"
    >
      {children}
    </a>
  );
}
