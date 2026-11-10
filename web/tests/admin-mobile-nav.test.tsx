// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminLayout } from "../src/features/admin/AdminLayout";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

vi.mock("../src/features/admin/useAdminEvents", () => ({ useAdminEvents: () => undefined }));

let container: HTMLDivElement;
let root: Root;

function render(path: string) {
  act(() => {
    root.render(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter initialEntries={[path]}>
          <Routes>
            <Route path="/admin" element={<AdminLayout />}>
              <Route index element={<div>dash</div>} />
              <Route path="*" element={<div>page</div>} />
            </Route>
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("admin mobile navigation", () => {
  it("shows four primary tabs and a more button in the bottom bar", () => {
    render("/admin");
    const bar = container.querySelector("nav.lux-admin-tabbar");
    expect(bar).not.toBeNull();
    const labels = [...bar!.querySelectorAll(".lux-admin-tab")].map((el) => el.textContent);
    expect(labels).toEqual(["仪表盘", "媒体库", "插件库", "通知", "更多"]);
  });

  it("opens a sheet with the secondary destinations and closes it on selection", () => {
    render("/admin");
    const more = container.querySelector<HTMLButtonElement>("button.lux-admin-tab-more")!;
    expect(more.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector(".lux-admin-more-sheet")).toBeNull();

    act(() => more.click());
    expect(more.getAttribute("aria-expanded")).toBe("true");
    const links = [...container.querySelectorAll(".lux-admin-more-sheet a")].map((el) => el.textContent);
    expect(links).toEqual(["用户与权限", "任务与日志", "服务器设置", "转码", "更新日志"]);

    act(() => container.querySelector<HTMLAnchorElement>(".lux-admin-more-sheet a")!.click());
    expect(container.querySelector(".lux-admin-more-sheet")).toBeNull();
  });

  it("highlights more when a secondary page is active", () => {
    render("/admin/users");
    expect(container.querySelector(".lux-admin-tab-more")?.classList.contains("is-active")).toBe(true);
  });
});
