// @vitest-environment jsdom

import { act, lazy, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { MediaEditorBoundary } from "../src/features/media/MediaEditorBoundary";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | undefined;
let container: HTMLDivElement;

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  vi.restoreAllMocks();
});

it("shows cancellable loading without hiding the detail page, then mounts the editor", async () => {
  let resolveEditor!: (value: { default: () => React.JSX.Element }) => void;
  const load = vi.fn(() => new Promise<{ default: () => React.JSX.Element }>((resolve) => { resolveEditor = resolve; }));
  const Editor = lazy(load);
  const close = vi.fn();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => root?.render(<><h1>电影详情</h1><MediaEditorBoundary onClose={close}><Editor /></MediaEditorBoundary></>));
  expect(load).toHaveBeenCalledOnce();
  expect(container.querySelector("h1")?.textContent).toBe("电影详情");
  expect(container.querySelector("[role=status]")?.textContent).toContain("正在加载编辑器");
  expect(document.activeElement?.getAttribute("aria-label")).toBe("关闭编辑器");
  act(() => document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })));
  expect(close).toHaveBeenCalledOnce();
  await act(async () => resolveEditor({ default: () => <section role="dialog">编辑器已就绪</section> }));
  expect(container.querySelector("[role=status]")).toBeNull();
  expect(container.querySelector("[role=dialog]")?.textContent).toBe("编辑器已就绪");
});

it("keeps the page usable after a rejected import and allows dismissing the error", async () => {
  vi.spyOn(console, "error").mockImplementation(() => undefined);
  const Editor = lazy(async () => { throw new Error("simulated chunk load failure"); });
  function Page() {
    const [open, setOpen] = useState(true);
    return <><h1>电影详情</h1>{open ? <MediaEditorBoundary onClose={() => setOpen(false)}><Editor /></MediaEditorBoundary> : null}</>;
  }
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => root?.render(<Page />));
  expect(container.querySelector("h1")?.textContent).toBe("电影详情");
  expect(container.querySelector("[role=alert]")?.textContent).toContain("编辑器加载失败");
  expect(container.textContent).not.toContain("simulated chunk load failure");
  act(() => container.querySelector<HTMLButtonElement>("[aria-label='关闭编辑器']")?.click());
  expect(container.querySelector("[role=alert]")).toBeNull();
  expect(document.body.style.overflow).not.toBe("hidden");
});
