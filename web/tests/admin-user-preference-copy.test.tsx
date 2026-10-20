// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUsersPage } from "../src/features/admin/AdminUsersPage";
import { api } from "../src/lib/api/client";
import type { AdminLibrary, AdminSettings, AdminUser } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function user(id: string, usernameNormalized: string, displayName: string): AdminUser {
  return {
    id,
    usernameNormalized,
    displayName,
    isDisabled: false,
    isAdmin: false,
    canManageServer: false,
    canRemoteAccess: false,
    canDownload: false,
  };
}

describe("AdminUsersPage account preference copy", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("previews selected groups and applies only selected targets with preview versions", async () => {
    const source = user("source", "source-user", "来源账号");
    const alice = user("alice", "alice", "爱丽丝");
    const bob = user("bob", "bob", "鲍勃");
    const unselected = user("unselected", "other", "未选择用户");
    const usersPage = { users: [source, alice, bob, unselected], page: 1, pageSize: 20, totalCount: 4 };
    const listUsers = vi.spyOn(api, "adminUsers").mockResolvedValue(usersPage);
    vi.spyOn(api, "adminLibraries").mockResolvedValue({
      libraries: [
        { id: "movies", name: "电影", kind: "MOVIE" } as AdminLibrary,
        { id: "series", name: "剧集", kind: "SERIES" } as AdminLibrary,
      ],
    });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [] });
    const preview = vi.spyOn(api, "previewAdminAccountPreferencesCopy").mockResolvedValue({
      sourceUserId: source.id,
      sourceVersion: 4,
      sourceSnapshot: {
        appearance: { theme: "dark", accentColor: "berry" },
        playback: { audioLanguage: "日语", subtitleLanguage: "简体中文", autoPlayNextEpisode: true, optimizedImages: true, playedPercent: 67 },
        libraryOrder: ["movies", "series"],
      },
      targets: [
        { userId: alice.id, expectedVersion: 7, libraryOrder: ["movies"] },
        { userId: bob.id, expectedVersion: 0, libraryOrder: ["movies", "series"] },
      ],
      groups: { appearance: true, home: false, playback: true, libraryOrder: true },
    });
    const apply = vi.spyOn(api, "copyAdminAccountPreferences").mockResolvedValue({
      sourceUserId: source.id,
      sourceVersion: 4,
      results: [
        { userId: alice.id, success: true, errorCode: null },
        { userId: bob.id, success: false, errorCode: "VERSION_CONFLICT" },
      ],
    });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("未选择用户"));
      container.querySelector<HTMLInputElement>('input[aria-label="选择爱丽丝"]')?.click();
      container.querySelector<HTMLInputElement>('input[aria-label="选择鲍勃"]')?.click();
    });
    expect(container.textContent).toContain("当前页已选 2 人");
    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="复制偏好"]')?.click();
      await vi.waitFor(() => expect(container.querySelector('[role="dialog"]')).toBeTruthy());
    });
    expect(container.textContent).toContain("爱丽丝");
    expect(container.textContent).toContain("鲍勃");
    expect(container.querySelector(".lux-admin-user-preference-copy-targets")?.textContent).not.toContain("未选择用户");

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('option[value="source"]')).toBeTruthy());
      const sourceSelect = container.querySelector<HTMLSelectElement>('select[aria-label="偏好复制来源"]');
      if (sourceSelect) {
        sourceSelect.value = source.id;
        sourceSelect.dispatchEvent(new Event("change", { bubbles: true }));
      }
      const home = container.querySelector<HTMLInputElement>('input[aria-label="复制首页偏好"]');
      home?.click();
      container.querySelector<HTMLButtonElement>('button[aria-label="预览偏好复制"]')?.click();
      await vi.waitFor(() => expect(preview).toHaveBeenCalledOnce());
    });
    expect(preview).toHaveBeenCalledWith({
      sourceUserId: source.id,
      targetUserIds: [alice.id, bob.id],
      groups: { appearance: true, home: false, playback: true, libraryOrder: true },
    });
    expect(container.textContent).toContain("深色");
    expect(container.textContent).toContain("莓果");
    expect(container.textContent).toContain("已看阈值 67%");
    expect(container.textContent).toContain("电影 → 剧集");
    expect(container.textContent).toContain("媒体库排序：电影");
    expect(container.textContent).toContain("版本 7");
    expect(container.textContent).toContain("版本 0");

    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="应用偏好复制"]')?.click();
      await vi.waitFor(() => expect(apply).toHaveBeenCalledOnce());
    });
    expect(apply).toHaveBeenCalledWith({
      sourceUserId: source.id,
      sourceExpectedVersion: 4,
      targets: [
        { userId: alice.id, expectedVersion: 7 },
        { userId: bob.id, expectedVersion: 0 },
      ],
      groups: { appearance: true, home: false, playback: true, libraryOrder: true },
    });
    expect(container.textContent).toContain("爱丽丝：成功");
    expect(container.textContent).toContain("鲍勃：偏好已被其他操作修改");
    expect(container.textContent).toContain("当前页已选 0 人");
    expect(listUsers.mock.calls.filter(([query]) => query?.pageSize === 20)).toHaveLength(2);
  });

  it("invalidates a preview when its groups change and requires a new preview after an apply error", async () => {
    const source = user("source", "source-user", "来源账号");
    const target = user("target", "target-user", "目标账号");
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [source, target], page: 1, pageSize: 20, totalCount: 2 });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [] });
    const preview = vi.spyOn(api, "previewAdminAccountPreferencesCopy").mockResolvedValue({
      sourceUserId: source.id,
      sourceVersion: 1,
      sourceSnapshot: {
        appearance: { theme: "light", accentColor: "silver" },
        playback: { audioLanguage: "原始音轨", subtitleLanguage: "简体中文", autoPlayNextEpisode: true, optimizedImages: false, playedPercent: 95 },
        libraryOrder: [],
      },
      targets: [{ userId: target.id, expectedVersion: 0 }],
      groups: { appearance: true, home: true, playback: true, libraryOrder: true },
    });
    const apply = vi.spyOn(api, "copyAdminAccountPreferences").mockRejectedValue(new Error("来源偏好已变更"));
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("目标账号"));
      container.querySelector<HTMLInputElement>('input[aria-label="选择目标账号"]')?.click();
      container.querySelector<HTMLButtonElement>('button[aria-label="复制偏好"]')?.click();
      await vi.waitFor(() => expect(container.querySelector('option[value="source"]')).toBeTruthy());
      const sourceSelect = container.querySelector<HTMLSelectElement>('select[aria-label="偏好复制来源"]');
      if (sourceSelect) {
        sourceSelect.value = source.id;
        sourceSelect.dispatchEvent(new Event("change", { bubbles: true }));
      }
      container.querySelector<HTMLButtonElement>('button[aria-label="预览偏好复制"]')?.click();
      await vi.waitFor(() => expect(preview).toHaveBeenCalledOnce());
    });

    await act(async () => {
      container.querySelector<HTMLInputElement>('input[aria-label="复制首页偏好"]')?.click();
    });
    expect(container.querySelector('button[aria-label="应用偏好复制"]')).toBeNull();
    expect(container.textContent).not.toContain("复制预览");

    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="预览偏好复制"]')?.click();
      await vi.waitFor(() => expect(preview).toHaveBeenCalledTimes(2));
      container.querySelector<HTMLButtonElement>('button[aria-label="应用偏好复制"]')?.click();
      await vi.waitFor(() => expect(apply).toHaveBeenCalledOnce());
    });
    expect(container.textContent).toContain("复制失败：来源偏好已变更");
    expect(container.querySelector('button[aria-label="应用偏好复制"]')).toBeNull();
    expect(container.querySelector('button[aria-label="预览偏好复制"]')).toBeTruthy();
  });

  function renderPage() {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(<QueryClientProvider client={queryClient}><MemoryRouter initialEntries={["/admin/users"]}><AdminUsersPage /></MemoryRouter></QueryClientProvider>));
  }
});
