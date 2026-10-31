import { Children, isValidElement, type ChangeEvent, type ReactNode } from "react";
import { LuxSelect, type LuxSelectOption } from "../LuxSelect";

/**
 * 把 `<option>` / `<optgroup>` 子节点折叠为 LuxSelect 的选项列表，
 * 让历史页面不必重写 options 数组即可切换到自研下拉。
 */
function collectOptions(node: ReactNode, into: LuxSelectOption[] = []): LuxSelectOption[] {
  Children.forEach(node, (child) => {
    if (!isValidElement(child)) return;
    if (typeof child.type === "string" && child.type === "option") {
      const props = child.props as { value?: string | number; disabled?: boolean; children?: ReactNode };
      into.push({ value: String(props.value ?? ""), label: props.children, disabled: props.disabled });
      return;
    }
    const nested = (child.props as { children?: ReactNode }).children;
    if (nested !== undefined) collectOptions(nested, into);
  });
  return into;
}

export type LuxSelectFieldProps = {
  value: string | readonly string[];
  /** 直接提供选项；与 children 二选一。 */
  options?: readonly LuxSelectOption[];
  children?: ReactNode;
  /**
   * 兼容原生 select 的事件签名，同时把新值作为第二个参数；
   * 这样从 `<select onChange={(event) => ...}>` 迁移时不必重写处理函数。
   */
  onChange?: (event: ChangeEvent<HTMLSelectElement>, value: string | readonly string[]) => void;
  multiple?: boolean;
  id?: string;
  name?: string;
  className?: string;
  menuClassName?: string;
  placeholder?: string;
  title?: string;
  disabled?: boolean;
  required?: boolean;
  "aria-label"?: string;
  "aria-labelledby"?: string;
};

function nativeEventFor(value: string | readonly string[]): ChangeEvent<HTMLSelectElement> {
  const list = Array.isArray(value) ? value : [value as string];
  const selectedOptions = list.map((item) => ({ value: item }) as unknown as HTMLOptionElement);
  const target = {
    value: list[0] ?? "",
    selectedOptions,
  } as unknown as EventTarget & HTMLSelectElement;
  return { target, currentTarget: target } as ChangeEvent<HTMLSelectElement>;
}

/**
 * LuxSelect 的声明式入口：接受原生 `<option>` 子节点与原生风格的
 * onChange，输出 Lux 自有 combobox 触发器和 portal 菜单。
 */
export function LuxSelectField({
  value,
  options,
  children,
  onChange,
  multiple,
  id,
  name,
  className,
  menuClassName,
  placeholder,
  disabled,
  title,
  ...aria
}: LuxSelectFieldProps) {
  const items = options ?? collectOptions(children);
  const isMultiple = multiple ?? Array.isArray(value);
  const emit = (next: string | string[]) => {
    onChange?.(nativeEventFor(next), next);
  };

  if (isMultiple) {
    const selected = Array.isArray(value) ? value : [];
    return (
      <LuxSelect
        multiple
        id={id}
        name={name}
        className={className}
        menuClassName={menuClassName}
        placeholder={placeholder}
        disabled={disabled}
        title={title}
        value={selected}
        options={items}
        onChange={(next) => emit(next)}
        aria-label={aria["aria-label"]}
        aria-labelledby={aria["aria-labelledby"]}
      />
    );
  }

  return (
    <LuxSelect
      id={id}
      name={name}
      className={className}
      menuClassName={menuClassName}
      placeholder={placeholder}
      disabled={disabled}
      title={title}
      value={String(value ?? "")}
      options={items}
      onChange={(next) => emit(next)}
      aria-label={aria["aria-label"]}
      aria-labelledby={aria["aria-labelledby"]}
    />
  );
}
