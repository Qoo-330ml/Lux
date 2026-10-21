// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type {
  AdminUser,
  AdminUserEmbySessionPage,
  AdminUserPlaybackSessionPage,
  AdminUserWebSessionPage,
} from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserDetailPage Web session management", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows safe session summaries and confirms revocation of only another session", async () => {
    const sessions: AdminUserWebSessionPage = {
      sessions: [
        { id: "current-session", createdAt: 1_700_000_000, updatedAt: 1_700_000_100, expiresAt: 1_702_000_000, lastSeenAt: 1_700_000_100, isCurrent: true },
        { id: "other-session", createdAt: 1_699_000_000, updatedAt: 1_699_000_100, expiresAt: 1_701_000_000, lastSeenAt: null, isCurrent: false },
      ],
      page: 1,
      pageSize: 20,
      totalCount: 2,
    };
    const refreshedSessions: AdminUserWebSessionPage = {
      ...sessions,
      sessions: [sessions.sessions[0]],
      totalCount: 1,
    };
    const list = vi.spyOn(api, "adminUserWebSessions")
      .mockResolvedValueOnce(sessions)
      .mockResolvedValueOnce(refreshedSessions);
    const revoke = vi.spyOn(api, "revokeAdminUserWebSession").mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    renderDevices();
    await waitForSessionTable();

    expect(container.textContent).toContain("2 个有效 Web 登录");
    expect(container.textContent).toContain("当前会话");
    expect(container.textContent).toContain("最近活动未知");
    expect(container.textContent).not.toContain("session-token");
    const rows = [...container.querySelectorAll<HTMLTableRowElement>("tbody tr")];
    expect(rows).toHaveLength(2);
    expect(rows.find((row) => row.textContent?.includes("当前会话"))?.querySelector("button")).toBeNull();

    const revokeButton = findButton("撤销登录");
    act(() => revokeButton.click());
    expect(confirm).toHaveBeenCalledWith("撤销此 Web 登录后，该浏览器下次访问时需要重新登录。确定撤销吗？");
    expect(revoke).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    await act(async () => {
      revokeButton.click();
      await vi.waitFor(() => expect(revoke).toHaveBeenCalledWith("user-1", "other-session"));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
    expect(container.textContent).toContain("Web 登录已撤销");
    expect(container.textContent).toContain("1 个有效 Web 登录");
  });

  it("paginates through active Web sessions", async () => {
    const firstPage: AdminUserWebSessionPage = {
      sessions: Array.from({ length: 20 }, (_, index) => makeSession(`session-${index + 1}`)),
      page: 1,
      pageSize: 20,
      totalCount: 21,
    };
    const secondPage: AdminUserWebSessionPage = {
      sessions: [makeSession("session-21")],
      page: 2,
      pageSize: 20,
      totalCount: 21,
    };
    const list = vi.spyOn(api, "adminUserWebSessions").mockImplementation(async (_userId, page = 1) => (
      page === 1 ? firstPage : secondPage
    ));
    renderDevices();
    await waitForSessionTable();
    expect(container.textContent).toContain("第 1 / 2 页");

    await act(async () => {
      findButton("下一页", ".lux-admin-user-web-sessions").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith("user-1", 2, 20));
      await vi.waitFor(() => expect(container.textContent).toContain("第 2 / 2 页"));
    });
    expect(container.querySelectorAll("tbody tr")).toHaveLength(1);
  });

  it("allows retry after a read error and shows an empty state", async () => {
    const list = vi.spyOn(api, "adminUserWebSessions")
      .mockRejectedValueOnce(new Error("temporary failure"))
      .mockResolvedValueOnce({ sessions: [], page: 1, pageSize: 20, totalCount: 0 });
    renderDevices();
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("temporary failure"));
    });

    await act(async () => {
      findButton("重试").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(container.textContent).toContain("当前没有有效的 Web 登录会话。"));
    });
  });

  it("shows safe Emby device summaries and confirms single credential revocation", async () => {
    const sessions: AdminUserEmbySessionPage = {
      sessions: [makeEmbySession("credential-1"), {
        ...makeEmbySession("credential-2"),
        deviceName: "Bedroom TV",
        lastSeenAt: null,
      }],
      page: 1,
      pageSize: 20,
      totalCount: 2,
    };
    const refreshedSessions: AdminUserEmbySessionPage = {
      ...sessions,
      sessions: [sessions.sessions[0]],
      totalCount: 1,
    };
    const list = vi.spyOn(api, "adminUserEmbySessions")
      .mockResolvedValueOnce(sessions)
      .mockResolvedValueOnce(refreshedSessions);
    const revoke = vi.spyOn(api, "revokeAdminUserEmbySession").mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    renderDevices();
    await waitForEmbySessionTable();

    expect(container.textContent).toContain("2 个有效 Emby 登录凭证");
    expect(container.textContent).toContain("Living Room");
    expect(container.textContent).toContain("设备 ID：device-credential-1");
    expect(container.textContent).toContain("Infuse 8.1");
    expect(container.textContent).toContain("最近活动未知");
    expect(container.textContent).not.toContain("access-token");
    expect(container.querySelectorAll(".lux-admin-user-emby-session-table tbody tr")).toHaveLength(2);

    const revokeButton = findButton("撤销登录");
    act(() => revokeButton.click());
    expect(confirm).toHaveBeenCalledWith("撤销此 Emby 登录凭证后，对应客户端需要重新登录。确定撤销吗？");
    expect(revoke).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    await act(async () => {
      revokeButton.click();
      await vi.waitFor(() => expect(revoke).toHaveBeenCalledWith("user-1", "credential-1"));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
    expect(container.textContent).toContain("Emby 登录已撤销。");
    expect(container.textContent).toContain("1 个有效 Emby 登录凭证");
  });

  it("shows current playback summaries and only stops confirmed server HLS sessions", async () => {
    const sessions: AdminUserPlaybackSessionPage = {
      sessions: [
        makePlaybackSession("hls-playback", {
          canStop: true,
          playbackPlan: "SERVER_HLS",
          playMethod: "Transcode",
          title: "Example Movie",
          positionTicks: 600_000_000,
          durationTicks: 1_200_000_000,
        }),
        makePlaybackSession("direct-playback", {
          canStop: false,
          playbackPlan: null,
          playMethod: "DirectPlay",
          title: "External STRM Movie",
          client: "External Player",
        }),
      ],
      page: 1,
      pageSize: 20,
      totalCount: 2,
    };
    const emptyPage: AdminUserPlaybackSessionPage = { ...sessions, sessions: [], totalCount: 0 };
    const list = vi.spyOn(api, "adminUserPlaybackSessions")
      .mockResolvedValueOnce(sessions)
      .mockResolvedValueOnce(emptyPage);
    const stop = vi.spyOn(api, "stopAdminUserPlaybackSession")
      .mockRejectedValueOnce(new Error("temporary stop failure"))
      .mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    renderDevices();
    await waitForPlaybackTable();

    expect(list).toHaveBeenCalledWith("user-1", 1, 20);
    expect(container.textContent).toContain("2 个活跃播放会话");
    expect(container.textContent).toContain("Example Movie");
    expect(container.textContent).toContain("50%");
    expect(container.textContent).toContain("Lux Web 1.0");
    expect(container.textContent).toContain("Living Room");
    expect(container.textContent).toContain("192.0.2.10");
    expect(container.textContent).toContain("External STRM Movie");
    expect(container.textContent).toContain("无法远程停止");
    expect(container.textContent).toContain("直放与 STRM 外部播放无法保证立即中断");
    expect(container.querySelectorAll(".lux-admin-user-playback-stop")).toHaveLength(1);

    const stopButton = findButton("停止播放");
    act(() => stopButton.click());
    expect(confirm).toHaveBeenCalledWith("将停止此用户通过 Lux 服务端 HLS 进行的播放。用户登录仍然有效。确定停止吗？");
    expect(stop).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    await act(async () => {
      stopButton.click();
      await vi.waitFor(() => expect(stop).toHaveBeenCalledWith("user-1", "hls-playback"));
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("temporary stop failure"));
    });
    expect(container.textContent).toContain("2 个活跃播放会话");

    await act(async () => {
      findButton("停止播放").click();
      await vi.waitFor(() => expect(stop).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(container.textContent).toContain("当前没有活跃的播放会话。"));
    });
    expect(container.textContent).toContain("播放已停止。");
  });

  it("paginates current playback and retries a failed session read", async () => {
    const firstPage: AdminUserPlaybackSessionPage = {
      sessions: Array.from({ length: 20 }, (_, index) => makePlaybackSession("playback-" + (index + 1), {
        canStop: false,
        title: "Movie " + (index + 1),
      })),
      page: 1,
      pageSize: 20,
      totalCount: 21,
    };
    const secondPage: AdminUserPlaybackSessionPage = {
      sessions: [makePlaybackSession("playback-21", { canStop: false, title: "Movie 21" })],
      page: 2,
      pageSize: 20,
      totalCount: 21,
    };
    const list = vi.spyOn(api, "adminUserPlaybackSessions")
      .mockRejectedValueOnce(new Error("temporary playback failure"))
      .mockResolvedValueOnce(firstPage)
      .mockResolvedValueOnce(secondPage);
    renderDevices();

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("temporary playback failure"));
    });
    await act(async () => {
      findButton("重试").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(container.textContent).toContain("第 1 / 2 页"));
    });
    await act(async () => {
      const pagination = container.querySelector('[aria-label="当前播放分页"]');
      const nextButton = [...pagination!.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.includes("下一页"));
      expect(nextButton).toBeTruthy();
      nextButton!.click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith("user-1", 2, 20));
      await vi.waitFor(() => expect(container.textContent).toContain("Movie 21"));
    });
    expect(container.textContent).toContain("第 2 / 2 页");
  });

  it("paginates active Emby login credentials", async () => {
    const firstPage: AdminUserEmbySessionPage = {
      sessions: Array.from({ length: 20 }, (_, index) => makeEmbySession("credential-" + (index + 1))),
      page: 1,
      pageSize: 20,
      totalCount: 21,
    };
    const secondPage: AdminUserEmbySessionPage = {
      sessions: [makeEmbySession("credential-21")],
      page: 2,
      pageSize: 20,
      totalCount: 21,
    };
    const list = vi.spyOn(api, "adminUserEmbySessions").mockImplementation(async (_userId, page = 1) => (
      page === 1 ? firstPage : secondPage
    ));
    renderDevices();
    await waitForEmbySessionTable();
    expect(container.textContent).toContain("第 1 / 2 页");

    const pagination = container.querySelector('[aria-label="Emby 登录分页"]');
    const nextButton = [...pagination!.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.includes("下一页"));
    expect(nextButton).toBeTruthy();
    await act(async () => {
      nextButton!.click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith("user-1", 2, 20));
      await vi.waitFor(() => expect(container.querySelector('[aria-label="Emby 登录分页"]')?.textContent).toContain("第 2 / 2 页"));
    });
    expect(container.querySelectorAll(".lux-admin-user-emby-session-table tbody tr")).toHaveLength(1);
  });

  it("returns to the previous Emby page when its last credential is revoked", async () => {
    const firstPage: AdminUserEmbySessionPage = {
      sessions: Array.from({ length: 20 }, (_, index) => makeEmbySession("credential-" + (index + 1))),
      page: 1,
      pageSize: 20,
      totalCount: 21,
    };
    const firstPageAfterRevoke: AdminUserEmbySessionPage = { ...firstPage, totalCount: 20 };
    const lastPage: AdminUserEmbySessionPage = {
      sessions: [makeEmbySession("credential-21")],
      page: 2,
      pageSize: 20,
      totalCount: 21,
    };
    let revoked = false;
    const list = vi.spyOn(api, "adminUserEmbySessions").mockImplementation(async (_userId, page = 1) => {
      if (page === 1) return revoked ? firstPageAfterRevoke : firstPage;
      return lastPage;
    });
    const revoke = vi.spyOn(api, "revokeAdminUserEmbySession").mockImplementation(async () => {
      revoked = true;
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);
    renderDevices();
    await waitForEmbySessionTable();

    const pagination = container.querySelector('[aria-label="Emby 登录分页"]');
    const nextButton = [...pagination!.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.includes("下一页"));
    await act(async () => {
      nextButton!.click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith("user-1", 2, 20));
      await vi.waitFor(() => expect(container.textContent).toContain("credential-21"));
    });

    await act(async () => {
      findButton("撤销登录").click();
      await vi.waitFor(() => expect(revoke).toHaveBeenCalledWith("user-1", "credential-21"));
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith("user-1", 1, 20));
      await vi.waitFor(() => expect(container.querySelector('[aria-label="Emby 登录分页"]')?.textContent).toContain("第 1 / 1 页"));
    });
    expect(container.querySelectorAll(".lux-admin-user-emby-session-table tbody tr")).toHaveLength(20);
  });

  it("confirms bulk Emby revocation, reports the revoked count, and refreshes", async () => {
    const populated: AdminUserEmbySessionPage = {
      sessions: [makeEmbySession("credential-1"), makeEmbySession("credential-2")],
      page: 1,
      pageSize: 20,
      totalCount: 2,
    };
    const empty: AdminUserEmbySessionPage = { ...populated, sessions: [], totalCount: 0 };
    const list = vi.spyOn(api, "adminUserEmbySessions")
      .mockResolvedValueOnce(populated)
      .mockResolvedValueOnce(empty);
    const revokeAll = vi.spyOn(api, "revokeAllAdminUserEmbySessions").mockResolvedValue({ revokedCount: 2 });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    renderDevices();
    await waitForEmbySessionTable();

    const revokeAllButton = findButton("撤销全部 Emby 登录");
    act(() => revokeAllButton.click());
    expect(confirm).toHaveBeenCalledWith("即将撤销此用户全部 2 个有效 Emby 登录凭证，对应客户端需要重新登录。确定继续吗？");
    expect(revokeAll).not.toHaveBeenCalled();

    confirm.mockReturnValue(true);
    await act(async () => {
      revokeAllButton.click();
      await vi.waitFor(() => expect(revokeAll).toHaveBeenCalledWith("user-1"));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
    expect(container.textContent).toContain("已撤销 2 个 Emby 登录凭证。");
    expect(container.textContent).toContain("当前没有有效的 Emby 登录凭证。");
  });

  it("reports Emby credential revoke errors and allows another attempt", async () => {
    const sessions: AdminUserEmbySessionPage = {
      sessions: [makeEmbySession("credential-1")],
      page: 1,
      pageSize: 20,
      totalCount: 1,
    };
    vi.spyOn(api, "adminUserEmbySessions").mockResolvedValue(sessions);
    const revoke = vi.spyOn(api, "revokeAdminUserEmbySession")
      .mockRejectedValueOnce(new Error("temporary revoke failure"))
      .mockResolvedValue(undefined);
    vi.spyOn(window, "confirm").mockReturnValue(true);
    renderDevices();
    await waitForEmbySessionTable();

    await act(async () => {
      findButton("撤销登录").click();
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("temporary revoke failure"));
    });
    expect(revoke).toHaveBeenCalledTimes(1);

    await act(async () => {
      findButton("撤销登录").click();
      await vi.waitFor(() => expect(revoke).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect([...container.querySelectorAll('[role="status"]')]
        .some((status) => status.textContent?.includes("Emby 登录已撤销"))).toBe(true));
    });
  });

  it("allows retry after an Emby session read error", async () => {
    const list = vi.spyOn(api, "adminUserEmbySessions")
      .mockRejectedValueOnce(new Error("temporary Emby failure"))
      .mockResolvedValueOnce({ sessions: [], page: 1, pageSize: 20, totalCount: 0 });
    renderDevices();
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("读取 Emby 登录失败：temporary Emby failure"));
    });

    await act(async () => {
      findButton("重试").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(container.textContent).toContain("当前没有有效的 Emby 登录凭证。"));
    });
  });

  function renderDevices() {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser() });
    if (!vi.isMockFunction(api.adminUserWebSessions)) {
      vi.spyOn(api, "adminUserWebSessions").mockResolvedValue({
        sessions: [],
        page: 1,
        pageSize: 20,
        totalCount: 0,
      });
    }
    if (!vi.isMockFunction(api.adminUserEmbySessions)) {
      vi.spyOn(api, "adminUserEmbySessions").mockResolvedValue({
        sessions: [],
        page: 1,
        pageSize: 20,
        totalCount: 0,
      });
    }
    if (!vi.isMockFunction(api.adminUserPlaybackSessions)) {
      vi.spyOn(api, "adminUserPlaybackSessions").mockResolvedValue({
        sessions: [],
        page: 1,
        pageSize: 20,
        totalCount: 0,
      });
    }
    if (!vi.isMockFunction(api.adminUserDevices)) {
      vi.spyOn(api, "adminUserDevices").mockResolvedValue({
        devices: [],
        page: 1,
        pageSize: 20,
        totalCount: 0,
      });
    }
    if (!vi.isMockFunction(api.adminUserAccessBlacklistRules)) {
      vi.spyOn(api, "adminUserAccessBlacklistRules").mockResolvedValue({
        rules: [],
        page: 1,
        pageSize: 20,
        totalCount: 0,
      });
    }
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/user-1?section=devices"]}>
          <Routes>
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
    return queryClient;
  }

  async function waitForSessionTable() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-session-table")).toBeTruthy());
    });
  }

  async function waitForEmbySessionTable() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-emby-session-table")).toBeTruthy());
    });
  }

  async function waitForPlaybackTable() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-playback-table")).toBeTruthy());
    });
  }

  function findButton(label: string, scopeSelector?: string) {
    const scope = scopeSelector ? container.querySelector(scopeSelector) : container;
    expect(scope, `scope ${scopeSelector ?? "container"}`).toBeTruthy();
    const button = [...scope!.querySelectorAll<HTMLButtonElement>("button")]
      .find((item) => item.textContent?.includes(label));
    expect(button, `button containing ${label}`).toBeTruthy();
    return button!;
  }
});

function makeUser(): AdminUser {
  return {
    id: "user-1",
    usernameNormalized: "viewer",
    displayName: "观众",
    isDisabled: false,
    isAdmin: false,
  };
}

function makeSession(id: string) {
  return {
    id,
    createdAt: 1_700_000_000,
    updatedAt: 1_700_000_100,
    expiresAt: 1_702_000_000,
    lastSeenAt: 1_700_000_100,
    isCurrent: false,
  };
}

function makeEmbySession(id: string) {
  return {
    id,
    deviceId: "device-" + id,
    deviceName: "Living Room",
    deviceType: "TV",
    clientName: "Infuse",
    clientVersion: "8.1",
    createdAt: 1_700_000_000,
    updatedAt: 1_700_000_100,
    lastSeenAt: 1_700_000_100 as number | null,
  };
}

function makePlaybackSession(id: string, overrides: Partial<AdminUserPlaybackSessionPage["sessions"][number]>) {
  return {
    id,
    userId: "user-1",
    userName: "Viewer",
    itemId: "item-" + id,
    title: "Example Movie",
    itemType: "MOVIE",
    posterAvailable: false,
    positionTicks: 0,
    durationTicks: 1_200_000_000,
    state: "PLAYING",
    isPaused: false,
    lastEventAt: 1_700_000_100,
    client: "Lux Web",
    clientVersion: "1.0",
    deviceId: "device-" + id,
    deviceName: "Living Room",
    deviceType: "Browser",
    remoteIp: "192.0.2.10",
    playSessionId: "play-session-" + id,
    playMethod: "DirectPlay",
    playbackPlan: null,
    canStop: false,
    ...overrides,
  };
}
