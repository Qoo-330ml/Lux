import { Check, ChevronDown } from "lucide-react";
import { createPortal } from "react-dom";
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";

export type LuxSelectOption = {
  value: string;
  label: ReactNode;
  disabled?: boolean;
};

type LuxSelectCommonProps = {
  id?: string;
  options: readonly LuxSelectOption[];
  className?: string;
  placeholder?: string;
  disabled?: boolean;
  "aria-label"?: string;
  "aria-labelledby"?: string;
};

export type LuxSelectProps = LuxSelectCommonProps & (
  | {
      multiple?: false;
      value: string;
      onChange: (value: string) => void;
    }
  | {
      multiple: true;
      value: readonly string[];
      onChange: (value: string[]) => void;
    }
);

export function LuxSelect(props: LuxSelectProps) {
  const {
    id,
    options,
    className,
    placeholder = "请选择",
    disabled = false,
    "aria-label": ariaLabel,
    "aria-labelledby": ariaLabelledBy,
  } = props;
  const multiple = props.multiple === true;
  const selectedValues = multiple ? props.value : [props.value];
  const generatedId = useId();
  const triggerId = id ?? `lux-select-${generatedId}`;
  const listboxId = `${triggerId}-listbox`;
  const selectedIndex = multiple ? -1 : options.findIndex((option) => option.value === props.value);
  const firstEnabledIndex = findEnabledIndex(options, 0, 1);
  const initialActiveIndex = selectedIndex >= 0 && !options[selectedIndex]?.disabled ? selectedIndex : firstEnabledIndex;
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(initialActiveIndex);
  const [menuPosition, setMenuPosition] = useState<CSSProperties>();
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const updateMenuPosition = useCallback(() => {
    const trigger = triggerRef.current;
    if (!trigger) return;
    const rect = trigger.getBoundingClientRect();
    const menuHeight = Math.min(260, Math.max(42, options.length * 40 + 10));
    const openUpwards = window.innerHeight - rect.bottom < menuHeight + 12 && rect.top > menuHeight + 12;
    const width = Math.max(rect.width, 160);
    const left = Math.min(Math.max(8, rect.left), Math.max(8, window.innerWidth - width - 8));
    setMenuPosition({
      top: openUpwards ? Math.max(8, rect.top - menuHeight - 6) : rect.bottom + 6,
      left,
      width,
      maxHeight: menuHeight,
    });
  }, [options.length]);

  useLayoutEffect(() => {
    if (!open) return;
    updateMenuPosition();
    const handleViewportChange = () => updateMenuPosition();
    window.addEventListener("resize", handleViewportChange);
    document.addEventListener("scroll", handleViewportChange, true);
    return () => {
      window.removeEventListener("resize", handleViewportChange);
      document.removeEventListener("scroll", handleViewportChange, true);
    };
  }, [open, updateMenuPosition]);

  useEffect(() => {
    if (!open) return;
    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (target && (triggerRef.current?.contains(target) || menuRef.current?.contains(target))) return;
      setOpen(false);
    };
    document.addEventListener("pointerdown", handlePointerDown);
    return () => document.removeEventListener("pointerdown", handlePointerDown);
  }, [open]);

  useEffect(() => {
    if (!open || !options[activeIndex]?.disabled) {
      return;
    }
    setActiveIndex(findEnabledIndex(options, activeIndex, 1));
  }, [activeIndex, open, options]);

  const openMenu = () => {
    if (disabled) return;
    setActiveIndex(initialActiveIndex);
    setOpen(true);
  };

  const selectOption = (index: number) => {
    const option = options[index];
    if (!option || option.disabled) return;
    if (multiple) {
      const nextValues = selectedValues.includes(option.value)
        ? selectedValues.filter((selectedValue) => selectedValue !== option.value)
        : [...selectedValues, option.value];
      props.onChange(nextValues);
      setActiveIndex(index);
      return;
    }
    props.onChange(option.value);
    setOpen(false);
    triggerRef.current?.focus();
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (!open && ["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key)) {
      event.preventDefault();
      openMenu();
      return;
    }
    if (!open) return;

    if (event.key === "Escape" || event.key === "Tab") {
      event.preventDefault();
      setOpen(false);
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const direction = event.key === "ArrowDown" ? 1 : -1;
      setActiveIndex(findEnabledIndex(options, activeIndex + direction, direction));
      return;
    }
    if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      setActiveIndex(findEnabledIndex(options, event.key === "Home" ? 0 : options.length - 1, event.key === "Home" ? 1 : -1));
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      selectOption(activeIndex);
    }
  };

  const selectedOptions = options.filter((option) => selectedValues.includes(option.value));
  const selectedOption = selectedIndex >= 0 ? options[selectedIndex] : undefined;
  const selectedLabel = multiple
    ? selectedOptions.length === 0
      ? placeholder
      : selectedOptions.length === 1
        ? selectedOptions[0]?.label
        : `${selectedOptions.length} 项已选择`
    : selectedOption?.label ?? placeholder;
  const rootClassName = ["lux-select", className].filter(Boolean).join(" ");
  const menu = open ? (
    <div
      ref={menuRef}
      className="lux-select-menu"
      id={listboxId}
      role="listbox"
      aria-multiselectable={multiple || undefined}
      aria-label={ariaLabelledBy ? undefined : ariaLabel}
      style={menuPosition}
    >
      {options.map((option, index) => {
        const selected = selectedValues.includes(option.value);
        const active = index === activeIndex;
        return (
          <button
            className={active ? "lux-select-option is-active" : "lux-select-option"}
            key={option.value}
            id={`${listboxId}-option-${index}`}
            type="button"
            role="option"
            aria-selected={selected}
            aria-disabled={option.disabled || undefined}
            data-value={option.value}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => selectOption(index)}
            disabled={option.disabled}
          >
            <span className="lux-select-option-label">{option.label}</span>
            {selected ? <Check className="lux-select-option-check" size={15} aria-hidden="true" /> : null}
          </button>
        );
      })}
    </div>
  ) : null;

  return (
    <div className={rootClassName} data-lux-select>
      <button
        ref={triggerRef}
        className="lux-select-trigger"
        id={triggerId}
        type="button"
        role="combobox"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listboxId}
        aria-activedescendant={open && activeIndex >= 0 ? `${listboxId}-option-${activeIndex}` : undefined}
        aria-label={ariaLabel}
        aria-labelledby={ariaLabelledBy}
        disabled={disabled}
        onClick={() => (open ? setOpen(false) : openMenu())}
        onKeyDown={handleKeyDown}
      >
        <span className="lux-select-value">{selectedLabel}</span>
        <ChevronDown className={open ? "lux-select-chevron is-open" : "lux-select-chevron"} size={16} aria-hidden="true" />
      </button>
      {typeof document === "undefined" || !menu ? null : createPortal(menu, document.body)}
    </div>
  );
}

function findEnabledIndex(options: readonly LuxSelectOption[], start: number, direction: 1 | -1) {
  if (!options.length) return -1;
  let index = start;
  for (let step = 0; step < options.length; step += 1) {
    if (index >= options.length) index = 0;
    if (index < 0) index = options.length - 1;
    if (!options[index]?.disabled) return index;
    index += direction;
  }
  return -1;
}
