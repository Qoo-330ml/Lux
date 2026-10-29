// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  AdminUserDetailPage,
  AdminUserSettingsPage,
} from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type {
  AccountPreferences,
  AdminMailSettingsResponse,
  AdminRegistrationSettings,
  AdminSettings,
  AdminUser,
} from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/**
 * Composite regression guard: the detail and settings pages are assembled from many
 * panels, so this test pins the panel composition (order, headings, section markup)
 * instead of the internals of any single panel.
 */
describe("admin user management page composition", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  const detailSections: Array<{ section: string; headings: string[] }> = [
    { section: "overview", headings: ["账户资料", "账号状态", "密码"] },
    { section: "access", headings: ["角色预设", "权限", "播放与转码", "用户内容限制", "内容分类索引"] },
    { section: "devices", headings: ["设备清单", "此用户访问黑名单", "Web 登录", "Emby 登录", "当前播放"] },
    { section: "activity", headings: ["活动记录"] },
    { section: "preferences", headings: ["账户偏好"] },
  ];

  it.each(detailSections)("assembles the $section section in a stable panel order", async ({ section, headings }) => {
    renderDetail(section);
    await act(async () => {
      await vi.waitFor(() => expect(detailHeadings()).toEqual(headings));
    });
    expect(container.querySelector(".lux-users-detail-body")).toBeTruthy();
    expect(container.querySelectorAll(".lux-users-detail > nav.lux-users-segments button")).toHaveLength(5);
    expect(container.querySelector(".lux-users-segments button[aria-current='page']")?.textContent).toBe(
      section === "overview" ? "概览" : section === "access" ? "访问权限" : section === "devices" ? "设备与访问" : section === "activity" ? "活动记录" : "偏好",
    );
  });

  it("keeps the detail header identity, badges and facts intact", async () => {
    renderDetail("overview");
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-users-detail-head")).toBeTruthy());
    });
    const head = container.querySelector(".lux-users-detail-head");
    expect(head?.querySelector(".lux-users-detail-name h1")?.textContent).toBe("观众");
    expect(head?.querySelector(".lux-user-badge")?.textContent).toBe("正常");
    expect(head?.querySelector(".lux-users-avatar.is-large")?.textContent).toBe("观");
    const facts = [...head!.querySelectorAll(".lux-users-detail-facts > span")].map((fact) => fact.textContent ?? "");
    expect(facts[0]).toBe("@viewer");
    expect(facts[1]).toBe("viewer@example.com（未验证）");
    expect(container.querySelector(".lux-users-detail-back")?.getAttribute("href")).toBe("/admin/users");
  });

  const settingsGroups: Array<{ section: string; headings: string[] }> = [
    { section: "registration", headings: ["注册与邀请", "SMTP 服务", "SMTP 身份验证", "连接检查"] },
    { section: "defaults", headings: ["角色预设", "新用户默认权限"] },
    { section: "blocklist", headings: ["全站黑名单", "可信代理"] },
    { section: "retention", headings: ["删除账号保留期限", "活动记录保留"] },
  ];

  it.each(settingsGroups)("assembles the $section policy group in a stable panel order", async ({ section, headings }) => {
    renderSettings(section);
    await act(async () => {
      await vi.waitFor(() => expect(settingsHeadings()).toEqual(headings));
    });
    expect(container.querySelector(".lux-users-pane > .lux-users-policy-body")).toBeTruthy();
  });

  it("keeps the mail settings panels separate while sharing one section heading", async () => {
    renderSettings("mail");
    await act(async () => {
      await vi.waitFor(() => expect(settingsHeadings()).toContain("SMTP 服务"));
    });
    expect(settingsHeadings()).toEqual(["注册与邀请", "SMTP 服务", "SMTP 身份验证", "连接检查"]);
    expect(container.querySelectorAll("#lux-admin-mail-settings-title, #lux-admin-mail-credentials-title, #lux-admin-mail-connection-title")).toHaveLength(3);
  });

  function detailHeadings() {
    return [...container.querySelectorAll<HTMLElement>(".lux-users-detail-body h2")].map((heading) => heading.textContent ?? "");
  }

  function settingsHeadings() {
    return [...container.querySelectorAll<HTMLElement>(".lux-users-policy-body h2")].map((heading) => heading.textContent ?? "");
  }

  function renderDetail(section: string) {
    mockDetailApis();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={[`/admin/users/user-1?section=${section}`]}>
          <Routes>
            <Route path="/admin/users" element={<div />} />
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
  }

  function renderSettings(section: string) {
    mockSettingsApis();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={[`/admin/users/settings?section=${section}`]}>
          <AdminUserSettingsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    ));
  }

  function mockDetailApis() {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser() });
    vi.spyOn(api, "adminSettings").mockResolvedValue(makeAdminSettings());
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "adminUserAccountPreferences").mockResolvedValue(makePreferences());
    vi.spyOn(api, "adminUserContentRestrictions").mockResolvedValue({ policy: null });
    vi.spyOn(api, "adminContentRestrictionIndexStatus").mockResolvedValue({ totalItems: 0, indexedItems: 0, pendingItems: 0 });
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [] });
    vi.spyOn(api, "adminUserLibraryAccess").mockResolvedValue({ libraryAccessMode: "ALL", libraryIds: [] });
    vi.spyOn(api, "adminUserPlaybackPolicy").mockResolvedValue({
      policy: { canPlay: true, maxSimultaneousPlaybacks: 2, allowRemux: true, allowAudioTranscode: true, allowVideoTranscode: true },
    });
    vi.spyOn(api, "adminUserDevices").mockResolvedValue({ devices: [], page: 1, pageSize: 20, totalCount: 0 });
    vi.spyOn(api, "adminUserAccessBlacklistRules").mockResolvedValue({ rules: [], page: 1, pageSize: 20, totalCount: 0 });
    vi.spyOn(api, "adminUserWebSessions").mockResolvedValue({ sessions: [], page: 1, pageSize: 20, totalCount: 0 });
    vi.spyOn(api, "adminUserEmbySessions").mockResolvedValue({ sessions: [], page: 1, pageSize: 20, totalCount: 0 });
    vi.spyOn(api, "adminUserPlaybackSessions").mockResolvedValue({ sessions: [], page: 1, pageSize: 20, totalCount: 0 });
    vi.spyOn(api, "adminUserActivity").mockResolvedValue({ events: [], page: 1, pageSize: 20, totalCount: 0 });
  }

  function mockSettingsApis() {
    vi.spyOn(api, "adminRegistrationSettings").mockResolvedValue({
      registrationMode: "INVITATION_ONLY",
      version: 1,
    } satisfies AdminRegistrationSettings);
    vi.spyOn(api, "adminMailSettings").mockResolvedValue({
      configured: false,
      credentialsConfigured: false,
      settings: null,
    } satisfies AdminMailSettingsResponse);
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "adminSettings").mockResolvedValue(makeAdminSettings());
    vi.spyOn(api, "adminGlobalAccessBlacklistRules").mockResolvedValue({ rules: [], page: 1, pageSize: 20, totalCount: 0 });
  }
});

function makeAdminSettings(): AdminSettings {
  return {
    userDeletionGraceDays: 7,
    userActivityRetentionDays: 90,
    newUserPermissions: {
      libraryAccessMode: "ALL",
      libraryIds: [],
      canDownload: true,
      canPlay: true,
      maxSimultaneousPlaybacks: 2,
      allowRemux: true,
      allowAudioTranscode: true,
      allowVideoTranscode: true,
    },
  } as AdminSettings;
}

function makeUser(): AdminUser {
  return {
    id: "user-1",
    usernameNormalized: "viewer",
    displayName: "观众",
    isDisabled: false,
    isAdmin: false,
    email: "viewer@example.com",
  };
}

function makePreferences(): AccountPreferences {
  return {
    theme: "dark",
    accentColor: "silver",
    showMediaLibraries: true,
    showContinueWatching: true,
    audioLanguage: "auto",
    subtitleLanguage: "auto",
    autoPlayNextEpisode: true,
    libraryOrder: [],
    playedPercent: 90,
    version: 1,
    updatedAt: null,
  };
}
