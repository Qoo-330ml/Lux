// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AdminUser, AdminUserPlaybackPolicy } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const defaultPolicy: AdminUserPlaybackPolicy = {
  canPlay: true,
  maxSimultaneousPlaybacks: 2,
  allowRemux: true,
  allowAudioTranscode: true,
  allowVideoTranscode: true,
};

describe("AdminUserDetailPage playback policy", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("loads the five settings and saves changes through the admin API", async () => {
    const updatedPolicy = { ...defaultPolicy, maxSimultaneousPlaybacks: 3, allowVideoTranscode: false };
    const getPolicy = vi.spyOn(api, "adminUserPlaybackPolicy").mockResolvedValue({ policy: defaultPolicy });
    const updatePolicy = vi.spyOn(api, "updateAdminUserPlaybackPolicy").mockResolvedValue({ policy: updatedPolicy });
    renderDetail();
    await waitForPolicyForm();

    expect(getPolicy).toHaveBeenCalledWith("user-1");
    expect(findLimit().value).toBe("2");
    expect(findCheckbox("允许播放").checked).toBe(true);
    expect(findCheckbox("允许封装转换").checked).toBe(true);
    expect(findCheckbox("允许音频转码").checked).toBe(true);
    expect(findCheckbox("允许视频转码").checked).toBe(true);
    expect(container.textContent).toContain("新建播放会话会按当前策略校验准入、并发和转码");
    expect(container.textContent).not.toContain("播放准入策略将在下一步生效");

    await act(async () => {
      changeInput(findLimit(), "3");
      findCheckbox("允许视频转码").click();
      findButton("保存播放策略").click();
      await vi.waitFor(() => expect(updatePolicy).toHaveBeenCalledWith("user-1", updatedPolicy));
    });
    expect(container.querySelector('[role="status"]')?.textContent).toBe("播放策略已保存。");
  });

  it("rejects negative limits while preserving zero as unlimited", async () => {
    const unlimitedPolicy = { ...defaultPolicy, maxSimultaneousPlaybacks: 0 };
    const updatePolicy = vi.spyOn(api, "updateAdminUserPlaybackPolicy").mockResolvedValue({ policy: unlimitedPolicy });
    vi.spyOn(api, "adminUserPlaybackPolicy").mockResolvedValue({ policy: defaultPolicy });
    renderDetail();
    await waitForPolicyForm();

    await act(async () => changeInput(findLimit(), "-1"));
    expect(findButton("保存播放策略").disabled).toBe(true);
    expect(container.textContent).toContain("请输入非负整数");

    await act(async () => changeInput(findLimit(), "0"));
    expect(container.textContent).toContain("0 表示不限");
    expect(findButton("保存播放策略").disabled).toBe(false);
    await act(async () => {
      findButton("保存播放策略").click();
      await vi.waitFor(() => expect(updatePolicy).toHaveBeenCalledWith("user-1", unlimitedPolicy));
    });
  });

  it("keeps the draft after a save error and shows deleted accounts as read-only", async () => {
    vi.spyOn(api, "adminUserPlaybackPolicy").mockResolvedValue({ policy: defaultPolicy });
    const updatePolicy = vi.spyOn(api, "updateAdminUserPlaybackPolicy").mockRejectedValue(new Error("网络错误"));
    renderDetail();
    await waitForPolicyForm();

    await act(async () => {
      changeInput(findLimit(), "4");
      findButton("保存播放策略").click();
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("网络错误"));
    });
    expect(findLimit().value).toBe("4");
    expect(updatePolicy).toHaveBeenCalledOnce();

    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();

    const getDeletedPolicy = vi.spyOn(api, "adminUserPlaybackPolicy");
    renderDetail(makeUser({ deletedAt: 1_800_000_000 }));
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("回收站中的账号播放策略为只读"));
    });
    expect(getDeletedPolicy).not.toHaveBeenCalled();
    expect(container.querySelector(".lux-admin-user-playback-form")).toBeNull();
  });

  function renderDetail(user = makeUser()) {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/user-1?section=playback"]}>
          <Routes>
            <Route path="/admin/users" element={<div />} />
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
            <Route path="*" element={<div>NO ROUTE</div>} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
  }

  async function waitForPolicyForm() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-playback-form")).toBeTruthy());
    });
  }

  function findLimit() {
    const input = container.querySelector<HTMLInputElement>('input[aria-label="同时播放上限"]');
    expect(input).toBeTruthy();
    return input!;
  }

  function findCheckbox(label: string) {
    const input = container.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);
    expect(input).toBeTruthy();
    return input!;
  }

  function findButton(label: string) {
    const button = [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.includes(label));
    expect(button, `button containing ${label}`).toBeTruthy();
    return button!;
  }

  function changeInput(input: HTMLInputElement, value: string) {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }
});

function makeUser(overrides: Partial<AdminUser> = {}): AdminUser {
  return {
    id: "user-1",
    usernameNormalized: "viewer",
    displayName: "观众",
    isDisabled: false,
    isAdmin: false,
    ...overrides,
  };
}
