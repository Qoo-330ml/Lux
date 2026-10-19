// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { ApiError, api } from "../src/lib/api/client";
import type {
  AdminContentRestrictionIndexStatus,
  AdminUser,
  AdminUserContentRestrictionPolicy,
} from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserDetailPage content restriction index status", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows partial index progress and explains the unfinished access behavior", async () => {
    const getStatus = vi.spyOn(api, "adminContentRestrictionIndexStatus").mockResolvedValue({
      totalItems: 8,
      indexedItems: 6,
      pendingItems: 2,
    });
    renderContentSection();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("6 / 8"));
    });

    expect(getStatus).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("待就绪 2 条");
    expect(container.textContent).toContain("正在回填");
    expect(container.textContent).toContain("访问拦截尚未接入");
    const progress = container.querySelector("progress");
    expect(progress?.value).toBe(6);
    expect(progress?.max).toBe(8);
  });

  it("shows an empty-library state when no media items need indexing", async () => {
    vi.spyOn(api, "adminContentRestrictionIndexStatus").mockResolvedValue({
      totalItems: 0,
      indexedItems: 0,
      pendingItems: 0,
    });
    renderContentSection();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("目前没有可统计的媒体条目"));
    });

    expect(container.textContent).toContain("暂无媒体");
  });

  it("shows an accessible loading state before status data arrives", async () => {
    let resolveStatus!: (status: AdminContentRestrictionIndexStatus) => void;
    const pending = new Promise<AdminContentRestrictionIndexStatus>((resolve) => {
      resolveStatus = resolve;
    });
    vi.spyOn(api, "adminContentRestrictionIndexStatus").mockReturnValue(pending);
    renderContentSection();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("正在读取内容索引进度"));
    });
    expect(container.querySelector('[role="status"]')).toBeTruthy();

    await act(async () => resolveStatus({ totalItems: 1, indexedItems: 1, pendingItems: 0 }));
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("1 / 1"));
    });

    expect(container.textContent).toContain("索引已就绪");
  });

  it("offers retry after an index status request fails", async () => {
    const getStatus = vi.spyOn(api, "adminContentRestrictionIndexStatus")
      .mockRejectedValueOnce(new Error("索引状态暂时不可用"))
      .mockResolvedValueOnce({ totalItems: 1, indexedItems: 1, pendingItems: 0 });
    renderContentSection();

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("索引状态暂时不可用"));
    });
    await act(async () => {
      findButton("重试").click();
      await vi.waitFor(() => expect(container.textContent).toContain("1 / 1"));
    });
    expect(getStatus).toHaveBeenCalledTimes(2);
  });

  function renderContentSection() {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser() });
    vi.spyOn(api, "adminUserContentRestrictions").mockResolvedValue({ policy: null });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/user-1?section=content"]}>
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

describe("AdminUserDetailPage content restriction policy", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows an unrestricted default for users without a saved policy", async () => {
    const getPolicy = vi.spyOn(api, "adminUserContentRestrictions").mockResolvedValue({ policy: null });
    renderContentSection();

    await waitForPolicyForm();

    expect(getPolicy).toHaveBeenCalledWith("user-1");
    expect(findSelect("分级体系").value).toBe("MPA");
    expect(findSelect("最高允许等级").value).toBe("");
    expect(findCheckbox("允许未分级内容").checked).toBe(true);
    expect(findButton("保存内容限制").disabled).toBe(true);
    expect(container.textContent).toContain("当前没有已保存的限制");
  });

  it("saves edited fields with the loaded CAS version", async () => {
    const existingPolicy: AdminUserContentRestrictionPolicy = {
      ratingSystem: "MPA",
      maximumRating: "PG",
      allowUnrated: true,
      allowedTags: ["family"],
      blockedTags: [],
      version: 7,
      updatedAt: 1_792_382_400,
    };
    vi.spyOn(api, "adminUserContentRestrictions").mockResolvedValue({ policy: existingPolicy });
    const updatePolicy = vi.spyOn(api, "updateAdminUserContentRestrictions").mockResolvedValue({
      policy: {
        ratingSystem: "TVPG",
        maximumRating: "TV-14",
        allowUnrated: false,
        allowedTags: ["animation", "family"],
        blockedTags: ["horror"],
        version: 8,
        updatedAt: 1_792_382_500,
      },
    });
    renderContentSection();
    await waitForPolicyForm();

    await act(async () => {
      changeSelect(findSelect("分级体系"), "TVPG");
    });
    await act(async () => {
      changeSelect(findSelect("最高允许等级"), "TV-14");
      findCheckbox("允许未分级内容").click();
      changeTextarea(findTextarea("允许标签"), "Animation\nFamily");
      changeTextarea(findTextarea("禁止标签"), "Horror");
      findButton("保存内容限制").click();
      await vi.waitFor(() => expect(updatePolicy).toHaveBeenCalledWith("user-1", {
        expectedVersion: 7,
        ratingSystem: "TVPG",
        maximumRating: "TV-14",
        allowUnrated: false,
        allowedTags: ["Animation", "Family"],
        blockedTags: ["Horror"],
      }));
    });

    expect(container.textContent).toContain("内容限制已保存。");
    expect(findSelect("分级体系").value).toBe("TVPG");
    expect(findSelect("最高允许等级").value).toBe("TV-14");
    expect(findTextarea("允许标签").value).toBe("animation\nfamily");
    expect(container.textContent).toContain("当前版本 8");
  });

  it("asks the administrator to reload after a version conflict", async () => {
    const first: AdminUserContentRestrictionPolicy = {
      ratingSystem: "MPA",
      maximumRating: "PG",
      allowUnrated: true,
      allowedTags: [],
      blockedTags: [],
      version: 2,
      updatedAt: 1_792_382_400,
    };
    const latest = { ...first, maximumRating: "R", version: 3, updatedAt: 1_792_382_500 };
    const getPolicy = vi.spyOn(api, "adminUserContentRestrictions")
      .mockResolvedValueOnce({ policy: first })
      .mockResolvedValueOnce({ policy: latest });
    vi.spyOn(api, "updateAdminUserContentRestrictions")
      .mockRejectedValue(new ApiError("版本冲突", { status: 409 }));
    renderContentSection();
    await waitForPolicyForm();

    await act(async () => {
      changeSelect(findSelect("最高允许等级"), "PG-13");
      findButton("保存内容限制").click();
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("其他管理员"));
    });
    expect(findButton("保存内容限制").disabled).toBe(true);

    await act(async () => {
      findButton("重新读取策略").click();
      await vi.waitFor(() => expect(findSelect("最高允许等级").value).toBe("R"));
    });
    expect(getPolicy).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("当前版本 3");
  });

  it("shows policy loading failures and offers retry", async () => {
    const getPolicy = vi.spyOn(api, "adminUserContentRestrictions")
      .mockRejectedValueOnce(new Error("策略服务暂时不可用"))
      .mockResolvedValueOnce({ policy: null });
    renderContentSection();

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("策略服务暂时不可用"));
    });
    await act(async () => {
      findButton("重试").click();
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-content-policy-form")).toBeTruthy());
    });
    expect(getPolicy).toHaveBeenCalledTimes(2);
  });

  it("keeps deleted users read-only", async () => {
    const getPolicy = vi.spyOn(api, "adminUserContentRestrictions");
    renderContentSection(makeUser({ deletedAt: 1_800_000_000 }));

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("回收站中的账号内容限制为只读"));
    });
    expect(getPolicy).not.toHaveBeenCalled();
    expect(container.querySelector(".lux-admin-user-content-policy-form")).toBeNull();
  });

  function renderContentSection(user = makeUser()) {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user });
    vi.spyOn(api, "adminContentRestrictionIndexStatus").mockResolvedValue({
      totalItems: 0,
      indexedItems: 0,
      pendingItems: 0,
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={[`/admin/users/${user.id}?section=content`]}>
          <Routes>
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
  }

  async function waitForPolicyForm() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-content-policy-form")).toBeTruthy());
    });
  }

  function findSelect(label: string) {
    const select = [...container.querySelectorAll<HTMLSelectElement>("select")]
      .find((item) => item.getAttribute("aria-label") === label);
    expect(select, `select labelled ${label}`).toBeTruthy();
    return select!;
  }

  function findCheckbox(label: string) {
    const input = container.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);
    expect(input).toBeTruthy();
    return input!;
  }

  function findTextarea(label: string) {
    const textarea = [...container.querySelectorAll<HTMLTextAreaElement>("textarea")]
      .find((item) => item.getAttribute("aria-label") === label);
    expect(textarea, `textarea labelled ${label}`).toBeTruthy();
    return textarea!;
  }

  function findButton(label: string) {
    const button = [...container.querySelectorAll<HTMLButtonElement>("button")]
      .find((item) => item.textContent?.includes(label));
    expect(button, `button containing ${label}`).toBeTruthy();
    return button!;
  }

  function changeSelect(select: HTMLSelectElement, value: string) {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  }

  function changeTextarea(textarea: HTMLTextAreaElement, value: string) {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set?.call(textarea, value);
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    textarea.dispatchEvent(new Event("change", { bubbles: true }));
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
