export type PolicyPreset = {
  value: string;
  label: string;
};

/** Image count tiers; 0 means "do not download this type". */
export const countPresets: PolicyPreset[] = [
  { value: "", label: "继承" },
  { value: "0", label: "0 张（不下载）" },
  { value: "1", label: "1 张" },
  { value: "2", label: "2 张" },
  { value: "3", label: "3 张" },
  { value: "4", label: "4 张" },
];

const flatCountPresets: PolicyPreset[] = countPresets.filter((preset) => preset.value !== "");

/**
 * Preset list for a stored value. A value that is not one of the tiers (for
 * example an older 7-image setting) is kept as its own option so opening the
 * form never silently rewrites it.
 */
export function presetsWithCurrent(
  presets: PolicyPreset[],
  value: number | undefined,
  options?: { allowInherit?: boolean },
): PolicyPreset[] {
  const allowInherit = options?.allowInherit ?? true;
  const base = allowInherit ? presets : presets.filter((preset) => preset.value !== "");
  if (value === undefined || value === null) return base;
  if (base.some((preset) => preset.value === String(value))) return base;
  return [...base, { value: String(value), label: `当前 ${value}` }];
}

export function flatCountOptions(value: number): PolicyPreset[] {
  return presetsWithCurrent(flatCountPresets, value, { allowInherit: false });
}
