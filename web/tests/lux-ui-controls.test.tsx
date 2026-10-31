// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  LuxButton,
  LuxCheckbox,
  LuxConfirmProvider,
  LuxDialog,
  LuxDisclosure,
  LuxIconButton,
  LuxProgress,
  LuxRadio,
  LuxSwitch,
  LuxSelectField,
  LuxTextarea,
  LuxTextField,
  useLuxConfirm,
} from "../src/components/ui";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("Lux UI controls", () => {
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
    document.body.innerHTML = "";
    vi.restoreAllMocks();
  });

  it("renders Lux actions with tone and size modifiers instead of ad-hoc class strings", () => {
    act(() => {
      root.render(
        <div>
          <LuxButton tone="primary" size="large">保存</LuxButton>
          <LuxButton tone="secondary" size="compact">取消</LuxButton>
          <LuxIconButton size="small" danger aria-label="删除" />
        </div>,
      );
    });

    const buttons = [...container.querySelectorAll("button")];
    expect(buttons[0]?.className).toContain("lux-button");
    expect(buttons[0]?.className).toContain("lux-button-primary");
    expect(buttons[0]?.className).toContain("lux-button-large");
    expect(buttons[1]?.className).toContain("lux-button-compact");
    expect(buttons[1]?.className).toContain("lux-button-secondary");
    expect(buttons[2]?.className).toContain("lux-icon-button");
    expect(buttons[2]?.className).toContain("lux-icon-button-small");
    expect(buttons[2]?.className).toContain("lux-danger-icon");
  });

  it("keeps native button semantics for disabled state and clicks", () => {
    const onClick = vi.fn();
    act(() => {
      root.render(
        <div>
          <LuxButton onClick={onClick}>执行</LuxButton>
          <LuxButton disabled>禁用</LuxButton>
        </div>,
      );
    });

    const [enabled, disabled] = [...container.querySelectorAll<HTMLButtonElement>("button")];
    expect(disabled?.disabled).toBe(true);
    act(() => enabled?.click());
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("renders text controls with the Lux field skin", () => {
    act(() => {
      root.render(
        <div>
          <LuxTextField aria-label="用户名" defaultValue="lux" />
          <LuxTextField aria-label="错误字段" invalid defaultValue="bad" />
          <LuxTextarea aria-label="备注" rows={3} />
        </div>,
      );
    });

    const [input, invalid, textarea] = [...container.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>("input, textarea")];
    expect(input?.className).toContain("lux-input");
    expect(invalid?.className).toContain("is-invalid");
    expect(textarea?.className).toContain("lux-input");
    expect(textarea?.tagName).toBe("TEXTAREA");
  });

  it("renders choices as native inputs wrapped by Lux visuals", () => {
    act(() => {
      root.render(
        <div>
          <LuxCheckbox defaultChecked label="启用通知" />
          <LuxRadio name="mode" value="a" defaultChecked label="模式 A" />
          <LuxSwitch label="自动续播" />
        </div>,
      );
    });

    const checkbox = container.querySelector<HTMLInputElement>("input[type='checkbox']");
    expect(checkbox?.className).toContain("lux-check-input");
    expect(checkbox?.checked).toBe(true);
    expect(container.querySelector("input[type='radio']")?.getAttribute("name")).toBe("mode");
    expect(container.querySelector("[role='switch']")).toBeTruthy();
    expect(container.textContent).toContain("启用通知");
  });

  it("renders progress with accessible values", () => {
    act(() => {
      root.render(<LuxProgress value={45} max={100} label="扫描进度" />);
    });

    const bar = container.querySelector("[role='progressbar']");
    expect(bar?.getAttribute("aria-valuenow")).toBe("45");
    expect(bar?.getAttribute("aria-valuemax")).toBe("100");
    expect(bar?.getAttribute("aria-label")).toBe("扫描进度");
    expect(container.querySelector<HTMLElement>(".lux-progress-fill")?.style.width).toBe("45%");
  });

  it("renders disclosure sections with Lux summary chrome", () => {
    act(() => {
      root.render(<LuxDisclosure summary="高级选项"><p>内容</p></LuxDisclosure>);
    });

    const details = container.querySelector("details");
    expect(details?.className).toContain("lux-disclosure");
    expect(details?.querySelector("summary")?.textContent).toContain("高级选项");
    expect(details?.textContent).toContain("内容");
  });

  it("lifts declared options into the Lux combobox", async () => {
    const onChange = vi.fn();
    act(() => {
      root.render(
        <LuxSelectField aria-label="编码器" value="auto" onChange={onChange}>
          <option value="auto">自动</option>
          <option value="nvenc">NVENC</option>
        </LuxSelectField>,
      );
    });

    expect(container.querySelector("select")).toBeNull();
    const trigger = container.querySelector<HTMLButtonElement>("[role='combobox']");
    expect(trigger?.textContent).toContain("自动");

    await act(async () => trigger?.click());
    const option = [...document.querySelectorAll<HTMLElement>("[role='option']")].find((item) => item.textContent?.includes("NVENC"));
    await act(async () => option?.click());
    expect(onChange.mock.calls[0]?.[1]).toBe("nvenc");
    expect((onChange.mock.calls[0]?.[0] as unknown as { target: { value: string } }).target.value).toBe("nvenc");
  });

  it("resolves confirm dialogs through Lux chrome", async () => {
    let accept: (() => Promise<boolean>) | undefined;
    act(() => {
      root.render(
        <LuxConfirmProvider>
          <ConfirmProbe onReady={(confirm) => { accept = () => confirm({ title: "删除账号", description: "无法恢复", confirmLabel: "删除", tone: "danger" }); }} />
        </LuxConfirmProvider>,
      );
    });

    let pending: Promise<boolean> | undefined;
    await act(async () => {
      pending = accept?.();
      await Promise.resolve();
    });

    const dialog = document.querySelector("[role='alertdialog']");
    expect(dialog).toBeTruthy();
    expect(dialog?.textContent).toContain("删除账号");
    expect(dialog?.textContent).toContain("无法恢复");

    await act(async () => {
      const button = [...(dialog?.querySelectorAll<HTMLButtonElement>("button") ?? [])].find((item) => item.textContent?.trim() === "删除");
      button?.click();
      await pending;
    });

    await expect(pending).resolves.toBe(true);
    expect(document.querySelector("[role='alertdialog']")).toBeNull();
  });

  it("resolves confirm dialogs as declined from the cancel action", async () => {
    let accept: (() => Promise<boolean>) | undefined;
    act(() => {
      root.render(
        <LuxConfirmProvider>
          <ConfirmProbe onReady={(confirm) => { accept = () => confirm("确定要禁用账户吗？"); }} />
        </LuxConfirmProvider>,
      );
    });

    let pending: Promise<boolean> | undefined;
    await act(async () => {
      pending = accept?.();
      await Promise.resolve();
    });

    const dialog = document.querySelector("[role='alertdialog']");
    expect(dialog?.textContent).toContain("确定要禁用账户吗？");

    await act(async () => {
      const button = [...(dialog?.querySelectorAll<HTMLButtonElement>("button") ?? [])].find((item) => item.textContent?.trim() === "取消");
      button?.click();
      await pending;
    });

    await expect(pending).resolves.toBe(false);
  });

  it("closes LuxDialog from the escape key", () => {
    const onClose = vi.fn();
    act(() => {
      root.render(<LuxDialog title="设置" onClose={onClose}><p>正文</p></LuxDialog>);
    });

    const dialog = document.querySelector("[role='dialog']");
    expect(dialog?.textContent).toContain("设置");
    act(() => {
      dialog?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(onClose).toHaveBeenCalled();
  });
});

function ConfirmProbe({ onReady }: { onReady: (confirm: (options: { title: string; description?: string; confirmLabel?: string; tone?: "primary" | "danger" }) => Promise<boolean>) => void }) {
  const confirm = useLuxConfirm();
  onReady(confirm);
  return null;
}
