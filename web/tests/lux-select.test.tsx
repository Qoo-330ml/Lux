// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LuxSelect } from "../src/components/LuxSelect";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LuxSelect", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("renders a styled listbox trigger instead of a native select", async () => {
    await act(async () => {
      root.render(
        <LuxSelect
          id="metadata-language"
          value="zh-CN"
          options={[{ value: "zh-CN", label: "简体中文" }, { value: "en-US", label: "English" }]}
          onChange={vi.fn()}
          aria-label="元数据语言"
        />,
      );
    });

    const trigger = container.querySelector<HTMLButtonElement>("[role='combobox']");
    expect(container.querySelector("select")).toBeNull();
    expect(trigger?.id).toBe("metadata-language");
    expect(trigger?.textContent).toContain("简体中文");
    expect(trigger?.getAttribute("aria-expanded")).toBe("false");
  });

  it("opens, selects an option, and closes when clicking outside", async () => {
    const onChange = vi.fn();

    await act(async () => {
      root.render(
        <LuxSelect
          value="zh-CN"
          options={[{ value: "zh-CN", label: "简体中文" }, { value: "en-US", label: "English" }]}
          onChange={onChange}
          aria-label="元数据语言"
        />,
      );
    });

    const trigger = container.querySelector<HTMLButtonElement>("[role='combobox']");
    await act(async () => trigger?.click());

    expect(trigger?.getAttribute("aria-expanded")).toBe("true");
    expect(document.querySelector("[role='listbox']")).toBeTruthy();
    expect(document.querySelector("[role='option'][aria-selected='true']")?.textContent).toContain("简体中文");

    const english = [...document.querySelectorAll<HTMLElement>("[role='option']")].find((option) => option.textContent?.includes("English"));
    await act(async () => english?.click());
    expect(onChange).toHaveBeenCalledWith("en-US");

    await act(async () => document.dispatchEvent(new Event("pointerdown")));
    expect(trigger?.getAttribute("aria-expanded")).toBe("false");
    expect(document.querySelector("[role='listbox']")).toBeNull();
  });

  it("supports keyboard selection and skips disabled options", async () => {
    const onChange = vi.fn();

    await act(async () => {
      root.render(
        <LuxSelect
          value="first"
          options={[
            { value: "first", label: "第一项" },
            { value: "disabled", label: "不可用", disabled: true },
            { value: "last", label: "最后一项" },
          ]}
          onChange={onChange}
          aria-label="选项"
        />,
      );
    });

    const trigger = container.querySelector<HTMLButtonElement>("[role='combobox']");
    await act(async () => trigger?.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true })));
    await act(async () => trigger?.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true })));
    expect(document.querySelector("[role='option'][aria-disabled='true']")).toBeTruthy();
    await act(async () => trigger?.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));

    expect(onChange).toHaveBeenCalledWith("last");
  });

  it("supports multiple selection without rendering a native select", async () => {
    const onChange = vi.fn();

    await act(async () => {
      root.render(
        <LuxSelect
          multiple
          value={["first"]}
          options={[{ value: "first", label: "第一项" }, { value: "second", label: "第二项" }]}
          onChange={onChange}
          aria-label="多选项"
        />,
      );
    });

    const trigger = container.querySelector<HTMLButtonElement>("[role='combobox']");
    expect(container.querySelector("select")).toBeNull();

    await act(async () => trigger?.click());

    const listbox = document.querySelector<HTMLElement>("[role='listbox']");
    expect(listbox?.getAttribute("aria-multiselectable")).toBe("true");

    const second = [...document.querySelectorAll<HTMLElement>("[role='option']")].find((option) => option.textContent?.includes("第二项"));
    await act(async () => second?.click());

    expect(onChange).toHaveBeenCalledWith(["first", "second"]);
    expect(trigger?.getAttribute("aria-expanded")).toBe("true");
  });
});
