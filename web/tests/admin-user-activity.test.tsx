// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import { chooseLuxOptionByValue, luxCombobox } from "./lux-select-helpers";
import type { AdminUser, AdminUserActivityPage } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserDetailPage activity timeline", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("filters and paginates safe activity summaries for this user", async () => {
    const firstPage: AdminUserActivityPage = {
      events: [
        {
          id: "event-1",
          userId: "user-1",
          actorUserId: "user-1",
          eventType: "PLAYBACK_STARTED",
          targetType: "media_item",
          targetId: "item-1",
          metadata: {
            client: "Lux",
            deviceName: "Web",
            state: "PLAYING",
            remoteIp: "192.0.2.10",
            accessToken: "must-not-render",
          },
          occurredAt: 1_792_000_000,
        },
        {
          id: "event-blacklist-hit",
          userId: "user-1",
          actorUserId: null,
          eventType: "USER_ACCESS_BLACKLIST_HIT",
          targetType: "playback_session",
          targetId: "session-1",
          metadata: { reason: "CLIENT", source: "EMBY_DIRECT_TICKET", signature: "must-not-render" },
          occurredAt: 1_792_000_001,
        },
      ],
      page: 1,
      pageSize: 20,
      totalCount: 21,
    };
    const secondPage: AdminUserActivityPage = {
      ...firstPage,
      events: [{ ...firstPage.events[0], id: "event-2", eventType: "AUTH_LOGIN" }],
      page: 2,
    };
    const list = vi.spyOn(api, "adminUserActivity").mockImplementation(async (_userId, query) => (
      query?.page === 2 ? secondPage : firstPage
    ));
    renderActivity();

    await act(async () => {
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith("user-1", { page: 1, pageSize: 20 }));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("访问黑名单命中"));
    });
    expect(container.textContent).toContain("开始播放");
    expect(container.textContent).toContain("访问黑名单命中");
    expect(container.textContent).toContain("命中原因");
    expect(container.textContent).toContain("CLIENT");
    expect(container.textContent).toContain("Web");
    expect(container.textContent).toContain("192.0.2.10");
    expect(container.textContent).not.toContain("must-not-render");
    expect(container.textContent).toContain("共 21 条");

    const eventType = luxCombobox(container, "事件类型");
    const from = container.querySelector<HTMLInputElement>("input[aria-label='开始时间']");
    const to = container.querySelector<HTMLInputElement>("input[aria-label='结束时间']");
    expect(eventType).toBeTruthy();
    expect(from).toBeTruthy();
    expect(to).toBeTruthy();
    await chooseLuxOptionByValue(container, "事件类型", "AUTH_LOGIN");
    await act(async () => {
      changeValue(from!, "2026-10-10T08:00");
    });
    await act(async () => {
      changeValue(to!, "2026-10-11T20:30");
    });
    await act(async () => {
      findButton("应用筛选").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith("user-1", {
        page: 1,
        pageSize: 20,
        eventType: "AUTH_LOGIN",
        from: Math.floor(new Date("2026-10-10T08:00").getTime() / 1000),
        to: Math.floor(new Date("2026-10-11T20:30").getTime() / 1000) + 59,
      }));
    });

    await act(async () => {
      findButton("下一页").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith("user-1", expect.objectContaining({ page: 2, eventType: "AUTH_LOGIN" })));
    });
    expect(container.textContent).toContain("第 2 / 2 页");
  });

  it("rejects reversed time ranges before reading and supports retry and empty state", async () => {
    const list = vi.spyOn(api, "adminUserActivity")
      .mockRejectedValueOnce(new Error("temporary activity failure"))
      .mockResolvedValueOnce({ events: [], page: 1, pageSize: 20, totalCount: 0 });
    renderActivity();
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("temporary activity failure"));
    });

    const from = container.querySelector<HTMLInputElement>("input[aria-label='开始时间']")!;
    const to = container.querySelector<HTMLInputElement>("input[aria-label='结束时间']")!;
    await act(async () => {
      changeValue(from, "2026-10-12T00:00");
    });
    await act(async () => {
      changeValue(to, "2026-10-11T00:00");
    });
    await act(async () => {
      findButton("应用筛选").click();
    });
    expect(container.textContent).toContain("开始时间不能晚于结束时间。");
    expect(list).toHaveBeenCalledTimes(1);

    await act(async () => {
      findButton("重试").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(container.textContent).toContain("没有符合条件的活动记录。"));
    });
  });

  function renderActivity() {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser() });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/user-1?section=activity"]}>
          <Routes>
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
  }

  function findButton(label: string) {
    const button = [...container.querySelectorAll<HTMLButtonElement>("button")]
      .find((item) => item.textContent?.includes(label));
    expect(button, `button containing ${label}`).toBeTruthy();
    return button!;
  }
});

function changeValue(element: HTMLInputElement | HTMLSelectElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), "value")?.set;
  setter?.call(element, value);
  element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
}

function makeUser(): AdminUser {
  return {
    id: "user-1",
    usernameNormalized: "viewer",
    displayName: "观众",
    isDisabled: false,
    isAdmin: false,
  };
}
