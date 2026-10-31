import type { ComponentPropsWithRef, Ref } from "react";
import { cx } from "./cx";

export type LuxTextFieldProps = ComponentPropsWithRef<"input"> & {
  /** 校验失败态：使用 Lux 的 accent/危险描边。 */
  invalid?: boolean;
  block?: boolean;
};

/** 单行文本输入；保留原生 input 语义，叠加 Lux 皮肤。 */
export function LuxTextField({ invalid = false, block = true, className, ref, ...rest }: LuxTextFieldProps) {
  return (
    <input
      {...rest}
      ref={ref as Ref<HTMLInputElement>}
      aria-invalid={invalid || undefined}
      className={cx("lux-input", block && "lux-input-block", invalid && "is-invalid", className)}
      data-lux-control="input"
    />
  );
}

export type LuxTextareaProps = ComponentPropsWithRef<"textarea"> & {
  invalid?: boolean;
};

/** 多行文本输入；保留原生 textarea 语义，叠加 Lux 皮肤。 */
export function LuxTextarea({ invalid = false, className, ref, ...rest }: LuxTextareaProps) {
  return (
    <textarea
      {...rest}
      ref={ref as Ref<HTMLTextAreaElement>}
      aria-invalid={invalid || undefined}
      className={cx("lux-input", "lux-textarea", invalid && "is-invalid", className)}
      data-lux-control="textarea"
    />
  );
}
