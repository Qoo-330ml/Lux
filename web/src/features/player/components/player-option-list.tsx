import { Check } from "lucide-react";

export type PlayerOption = {
  value: string;
  label: string;
  /** Short secondary text on the right, such as a codec or source kind. */
  note?: string | null;
  disabled?: boolean;
};

type PlayerOptionListProps = {
  /** Accessible name of the whole group, for example 选择字幕. */
  label: string;
  options: readonly PlayerOption[];
  value: string;
  disabled?: boolean;
  onSelect: (value: string) => void;
};

/**
 * One-of-many choice shown as a list of rows with the current row highlighted. It replaces the
 * dropdown selects in the settings drawer and the track popovers; the group exposes the selected
 * value through `data-value`.
 */
export function PlayerOptionList({ label, options, value, disabled = false, onSelect }: PlayerOptionListProps) {
  return (
    <div
      className="lux-player-option-list"
      role="radiogroup"
      aria-label={label}
      aria-disabled={disabled || undefined}
      data-value={value}
    >
      {options.map((option) => {
        const selected = option.value === value;
        return (
          <button
            key={option.value === "" ? "__empty" : option.value}
            type="button"
            role="radio"
            aria-checked={selected}
            data-value={option.value}
            className={`lux-player-option${selected ? " is-selected" : ""}`}
            disabled={disabled || option.disabled}
            onClick={() => onSelect(option.value)}
          >
            <span className="lux-player-option-label">{option.label}</span>
            {option.note ? <em className="lux-player-option-note">{option.note}</em> : null}
            {selected ? <Check className="lux-player-option-check" size={15} aria-hidden="true" /> : null}
          </button>
        );
      })}
    </div>
  );
}
