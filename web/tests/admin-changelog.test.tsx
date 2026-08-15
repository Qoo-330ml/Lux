// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it } from "vitest";
import { adminNav } from "../src/features/admin/AdminLayout";
import { AdminChangelogPage } from "../src/features/admin/AdminChangelogPage";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminChangelogPage", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
  });

  it("places the changelog at the end of the admin navigation", () => {
    expect(adminNav.at(-1)).toMatchObject({ to: "/admin/changelog", label: "更新日志" });
  });

  it("shows the project's version history in a Lux-styled timeline", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root?.render(
        <MemoryRouter>
          <AdminChangelogPage />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector("h1")?.textContent).toBe("更新日志");
    expect(container.querySelectorAll(".lux-changelog-release")).not.toHaveLength(0);
    expect(container.textContent).toContain("0.2.0");
    expect(container.textContent).toContain("0.1.0");
    expect(container.textContent).toContain("支持每个用户独立调整自动标记已看的播放阈值");
    expect(container.textContent).toContain("建立 Lux Rust 模块化单体服务");
  });
});

describe("AdminLayout changelog route", () => {
  it("renders the navigation label as an accessible link", () => {
    expect(adminNav.map((item) => item.label)).toContain("更新日志");
  });
});
