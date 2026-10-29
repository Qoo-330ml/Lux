import type { ComponentPropsWithRef, ReactNode, Ref } from "react";
import { cx } from "./cx";

type ChoiceBaseProps = {
  label?: ReactNode;
  description?: ReactNode;
  className?: string;
};

export type LuxCheckboxProps = Omit<ComponentPropsWithRef<"input">, "type"> & ChoiceBaseProps;

/** 复选框：原生 input 承载状态与可访问性，Lux 负责勾选框外观。 */
export function LuxCheckbox({ label, description, className, ref, ...rest }: LuxCheckboxProps) {
  return (
    <label className={cx("lux-choice", "lux-checkbox", rest.disabled && "is-disabled", className)} data-lux-control="checkbox">
      <input {...rest} ref={ref as Ref<HTMLInputElement>} type="checkbox" className="lux-check-input" />
      <span className="lux-choice-box" aria-hidden="true" />
      {label === undefined ? null : <span className="lux-choice-label">{label}</span>}
      {description === undefined ? null : <span className="lux-choice-description">{description}</span>}
    </label>
  );
}

export type LuxRadioProps = Omit<ComponentPropsWithRef<"input">, "type"> & ChoiceBaseProps;

/** 单选框：原生 input 承载状态与可访问性，Lux 负责圆点外观。 */
export function LuxRadio({ label, description, className, ref, ...rest }: LuxRadioProps) {
  return (
    <label className={cx("lux-choice", "lux-radio", rest.disabled && "is-disabled", className)} data-lux-control="radio">
      <input {...rest} ref={ref as Ref<HTMLInputElement>} type="radio" className="lux-check-input" />
      <span className="lux-choice-box" aria-hidden="true" />
      {label === undefined ? null : <span className="lux-choice-label">{label}</span>}
      {description === undefined ? null : <span className="lux-choice-description">{description}</span>}
    </label>
  );
}

export type LuxSwitchProps = Omit<ComponentPropsWithRef<"input">, "type"> & ChoiceBaseProps;

/** 开关：原生 checkbox + role="switch"，轨道与滑块由 Lux 绘制。 */
export function LuxSwitch({ label, description, className, ref, ...rest }: LuxSwitchProps) {
  return (
    <label className={cx("lux-choice", "lux-switch", rest.disabled && "is-disabled", className)} data-lux-control="switch">
      <input {...rest} ref={ref as Ref<HTMLInputElement>} type="checkbox" role="switch" className="lux-switch-input" />
      <span className="lux-switch-track" aria-hidden="true">
        <span className="lux-switch-thumb" />
      </span>
      {label === undefined ? null : <span className="lux-choice-label">{label}</span>}
      {description === undefined ? null : <span className="lux-choice-description">{description}</span>}
    </label>
  );
}
