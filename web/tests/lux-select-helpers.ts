import { act } from "react";

/** 只取最新打开的 listbox，避免其他测试残留的 portal 菜单干扰。 */
function currentOptions(): HTMLElement[] {
  const lists = [...document.querySelectorAll<HTMLElement>('[role="listbox"]')];
  const list = lists[lists.length - 1];
  if (!list) return [];
  return [...list.querySelectorAll<HTMLElement>('[role="option"]')];
}

export type LuxSelectTarget = string | HTMLElement;

function resolveTrigger(container: ParentNode, target: LuxSelectTarget): HTMLElement | null {
  if (typeof target !== "string") return target;
  const candidates = [...container.querySelectorAll<HTMLElement>('[role="combobox"]')];
  return candidates.find((element) => element.getAttribute("aria-label") === target) ?? null;
}

/** 查找 LuxSelect 的 combobox 触发器（按 aria-label 精确匹配）。 */
export function luxCombobox(container: ParentNode, label: string): HTMLElement | null {
  return resolveTrigger(container, label);
}

/** 读取 LuxSelect 当前值。 */
export function luxSelectValue(container: ParentNode, label: LuxSelectTarget): string | undefined {
  return luxCombobox(container, label)?.dataset.value;
}

/** 打开 LuxSelect 并点选包含指定文案的选项。 */
export async function chooseLuxOption(container: ParentNode, label: LuxSelectTarget, optionLabel: string): Promise<void> {
  const trigger = resolveTrigger(container, label);
  if (!trigger) throw new Error(`未找到 LuxSelect：${typeof label === "string" ? label : label.id}`);
  await act(async () => {
    trigger.click();
    await Promise.resolve();
  });
  const options = currentOptions();
  const exact = options.find((option) => option.textContent?.trim() === optionLabel);
  const target = exact ?? options.find((option) => option.textContent?.includes(optionLabel));
  if (!target) throw new Error(`未找到选项：${optionLabel}`);
  await act(async () => {
    target.click();
    await Promise.resolve();
  });
}

/** 读取 LuxSelect 的选项值列表（打开后立即关闭）。 */
export async function luxOptionValues(container: ParentNode, label: LuxSelectTarget): Promise<string[]> {
  const trigger = resolveTrigger(container, label);
  if (!trigger) throw new Error(`未找到 LuxSelect：${typeof label === "string" ? label : label.id}`);
  await act(async () => {
    trigger.click();
    await Promise.resolve();
  });
  const values = currentOptions().map((option) => option.dataset.value ?? "");
  await act(async () => {
    trigger.click();
    await Promise.resolve();
  });
  return values;
}

/** 按选项值（而非文案）点选，等价于旧测试里的 select.value = value。 */
export async function chooseLuxOptionByValue(container: ParentNode, label: LuxSelectTarget, value: string): Promise<void> {
  const trigger = resolveTrigger(container, label);
  if (!trigger) throw new Error(`未找到 LuxSelect：${typeof label === "string" ? label : label.id}`);
  await act(async () => {
    trigger.click();
    await Promise.resolve();
  });
  const option = currentOptions().find((candidate) => candidate.dataset.value === value);
  if (!option) throw new Error(`未找到选项值：${value}`);
  await act(async () => {
    option.click();
    await Promise.resolve();
  });
}

export type LuxOptionSnapshot = { value: string; label: string; disabled: boolean };

/** 读取 LuxSelect 的选项快照（值、文案、禁用态），读取后关闭菜单。 */
export async function luxOptions(container: ParentNode, label: LuxSelectTarget): Promise<LuxOptionSnapshot[]> {
  const trigger = resolveTrigger(container, label);
  if (!trigger) throw new Error(`未找到 LuxSelect：${typeof label === "string" ? label : label.id}`);
  await act(async () => {
    trigger.click();
    await Promise.resolve();
  });
  const items = currentOptions().map((option) => ({
    value: option.dataset.value ?? "",
    label: option.textContent?.trim() ?? "",
    disabled: option.getAttribute("aria-disabled") === "true",
  }));
  await act(async () => {
    trigger.click();
    await Promise.resolve();
  });
  return items;
}
