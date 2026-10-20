// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminUsersPage } from "../src/features/admin/AdminUsersPage";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";
import type { AdminLibrary, AdminUserRegistration, NewUserPermissions } from "../src/lib/api/types";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const permissions: NewUserPermissions = {
  libraryAccessMode: "SELECTED",
  libraryIds: ["library-1"],
  canDownload: false,
  canPlay: true,
  maxSimultaneousPlaybacks: 2,
  allowRemux: true,
  allowAudioTranscode: true,
  allowVideoTranscode: false,
};

const registration: AdminUserRegistration = {
  id: "registration-1",
  recipientEmail: "person@example.test",
  status: "PENDING_APPROVAL",
  createdAt: 1_790_000_000,
  emailVerifiedAt: 1_790_000_100,
  permissions,
};

describe("admin user registration review", () => {
  let container: HTMLDivElement;
  let root: Root;
  let queryClient: QueryClient;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
    vi.spyOn(api, "adminUserRegistrations").mockResolvedValue({ requests: [], page: 1, pageSize: 20, totalCount: 0 });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [
      { id: "library-1", name: "电影库", isEnabled: true } as AdminLibrary,
    ] });
  });

  afterEach(() => {
    act(() => root.unmount());
    queryClient.clear();
    container.remove();
    vi.restoreAllMocks();
  });

  it("loads verified pending requests, retries sanitized list errors, and paginates server results", async () => {
    vi.mocked(api.adminUserRegistrations)
      .mockRejectedValueOnce(new Error("internal database detail"))
      .mockResolvedValueOnce({ requests: [registration], page: 1, pageSize: 20, totalCount: 21 })
      .mockResolvedValueOnce({ requests: [], page: 2, pageSize: 20, totalCount: 21 });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')).not.toBeNull());
    });
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("无法读取待审批申请");
    expect(container.textContent).not.toContain("internal database detail");
    await act(async () => {
      button("重新加载申请").click();
      await vi.waitFor(() => expect(container.textContent).toContain("person@example.test"));
    });
    expect(api.adminUserRegistrations).toHaveBeenNthCalledWith(1, { page: 1, pageSize: 20 });
    expect(api.adminUserRegistrations).toHaveBeenNthCalledWith(2, { page: 1, pageSize: 20 });

    await act(async () => {
      button("待审批下一页").click();
      await vi.waitFor(() => expect(api.adminUserRegistrations).toHaveBeenCalledWith({ page: 2, pageSize: 20 }));
    });
    expect(container.textContent).toContain("第 2 / 2 页");
    expect(container.textContent).toContain("此页没有待审批申请");
  });

  it("announces loading while the server page is pending and shows the empty state", async () => {
    let resolvePage!: (page: { requests: AdminUserRegistration[]; page: number; pageSize: number; totalCount: number }) => void;
    vi.mocked(api.adminUserRegistrations).mockImplementationOnce(() => new Promise((resolve) => { resolvePage = resolve; }));
    renderPage();
    expect(container.querySelector('[role="status"]')?.textContent).toContain("正在读取待审批申请");
    expect(container.querySelector("table")).toBeNull();
    await act(async () => {
      resolvePage({ requests: [], page: 1, pageSize: 20, totalCount: 0 });
      await vi.waitFor(() => expect(container.textContent).toContain("目前没有待审批申请。"));
    });
  });

  it("requires disabled snapshot libraries to be removed before approval", async () => {
    vi.mocked(api.adminUserRegistrations).mockResolvedValueOnce({ requests: [registration], page: 1, pageSize: 20, totalCount: 1 });
    vi.mocked(api.adminLibraries).mockResolvedValueOnce({ libraries: [
      { id: "library-1", name: "电影库", isEnabled: false } as AdminLibrary,
    ] });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("当前不可用"));
    });
    expect(button("批准 person@example.test").disabled).toBe(true);
    await act(async () => {
      setChecked('input[aria-label="媒体库 电影库 person@example.test"]', false);
      await vi.waitFor(() => expect(button("批准 person@example.test").disabled).toBe(false));
    });
  });

  it("shows the submitted permission snapshot, allows edits, confirms approval, and keeps failures safe", async () => {
    vi.mocked(api.adminUserRegistrations)
      .mockResolvedValueOnce({ requests: [registration], page: 1, pageSize: 20, totalCount: 1 })
      .mockResolvedValue({ requests: [], page: 1, pageSize: 20, totalCount: 0 });
    const approve = vi.spyOn(api, "approveAdminUserRegistration")
      .mockRejectedValueOnce(new Error("private response with secret"))
      .mockResolvedValue({ approved: true, userId: "user-1" });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    queryClient.setQueryData(queryKeys.adminUsers, { users: [], page: 1, pageSize: 20, totalCount: 0 });
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("person@example.test"));
    });
    expect(container.textContent).toContain("申请时权限快照");
    expect(container.textContent).toContain("电影库");
    expect(container.textContent).toContain("最多同时播放 2 路");

    await act(async () => {
      setChecked('input[aria-label="允许下载 person@example.test"]', true);
      await vi.waitFor(() => expect(container.querySelector<HTMLInputElement>('input[aria-label="允许下载 person@example.test"]')?.checked).toBe(true));
      setChecked('input[aria-label="允许播放 person@example.test"]', false);
      setChecked('input[aria-label="允许视频转码 person@example.test"]', true);
      setSelect('select[aria-label="媒体库访问范围 person@example.test"]', "ALL");
      setNumber('input[aria-label="同时播放上限 person@example.test"]', "3");
    });
    await act(async () => {
      button("批准 person@example.test").click();
      await vi.waitFor(() => expect(approve).toHaveBeenCalledTimes(1));
    });
    expect(approve).toHaveBeenCalledWith(registration.id, {
      permissions: {
        ...permissions,
        libraryAccessMode: "ALL",
        libraryIds: [],
        canDownload: true,
        canPlay: false,
        maxSimultaneousPlaybacks: 3,
        allowVideoTranscode: true,
      },
    });
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain("审批失败，申请仍保留");
    expect(container.textContent).not.toContain("private response with secret");
    expect(container.textContent).toContain("person@example.test");

    await act(async () => {
      button("批准 person@example.test").click();
      await vi.waitFor(() => expect(approve).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(api.adminUserRegistrations).toHaveBeenCalledTimes(2));
    });
    expect(confirm).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("注册申请已批准，用户已创建。");
    expect(queryClient.getQueryState(queryKeys.adminUsers)?.isInvalidated).toBe(true);
    expect(container.textContent).not.toContain("person@example.test");
  });

  it("requires confirmation before rejection and refreshes the pending list", async () => {
    vi.mocked(api.adminUserRegistrations)
      .mockResolvedValueOnce({ requests: [registration], page: 1, pageSize: 20, totalCount: 1 })
      .mockResolvedValue({ requests: [], page: 1, pageSize: 20, totalCount: 0 });
    const reject = vi.spyOn(api, "rejectAdminUserRegistration").mockResolvedValue({ rejected: true });
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false).mockReturnValue(true);
    renderPage();

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("person@example.test"));
      button("拒绝 person@example.test").click();
      expect(reject).not.toHaveBeenCalled();
      button("拒绝 person@example.test").click();
      await vi.waitFor(() => expect(reject).toHaveBeenCalledWith(registration.id));
      await vi.waitFor(() => expect(api.adminUserRegistrations).toHaveBeenCalledTimes(2));
    });
    expect(confirm).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("注册申请已拒绝。");
    expect(container.textContent).toContain("目前没有待审批申请。");
  });

  function renderPage(initialEntry = "/admin/users?view=pending") {
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={[initialEntry]}>
          <AdminUsersPage />
        </MemoryRouter>
      </QueryClientProvider>,
    ));
  }

  function button(label: string) {
    const found = Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
      .find((item) => item.textContent?.trim() === label || item.getAttribute("aria-label") === label);
    if (!found) throw new Error(`Missing button: ${label}`);
    return found;
  }

  function setChecked(selector: string, checked: boolean) {
    const input = container.querySelector<HTMLInputElement>(selector);
    if (!input) throw new Error(`Missing input: ${selector}`);
    if (input.checked !== checked) input.click();
  }

  function setSelect(selector: string, value: string) {
    const select = container.querySelector<HTMLSelectElement>(selector);
    if (!select) throw new Error(`Missing select: ${selector}`);
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set?.call(select, value);
    select.dispatchEvent(new Event("change", { bubbles: true }));
  }

  function setNumber(selector: string, value: string) {
    const input = container.querySelector<HTMLInputElement>(selector);
    if (!input) throw new Error(`Missing input: ${selector}`);
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }
});
