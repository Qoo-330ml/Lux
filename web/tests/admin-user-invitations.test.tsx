// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUsersPage } from "../src/features/admin/AdminUsersPage";
import { api } from "../src/lib/api/client";
import { chooseLuxOptionByValue, luxCombobox } from "./lux-select-helpers";
import type { AdminLibrary, AdminSettings, AdminUserInvitation, AdminUserPermissionPreset } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const preset: AdminUserPermissionPreset = {
  id: "preset-family",
  name: "家庭用户",
  description: null,
  version: 3,
  isBuiltin: false,
  isAdmin: false,
  canManageServer: false,
  permissions: {
    libraryAccessMode: "SELECTED",
    libraryIds: ["library-movies"],
    canDownload: false,
    canPlay: true,
    maxSimultaneousPlaybacks: 2,
    allowRemux: true,
    allowAudioTranscode: true,
    allowVideoTranscode: false,
  },
};

const invitation: AdminUserInvitation = {
  id: "invitation-1",
  recipientEmail: "person@example.test",
  createdByUserId: "admin-1",
  acceptedUserId: null,
  status: "PENDING",
  presetId: preset.id,
  presetName: preset.name,
  presetVersion: preset.version,
  expiresAt: 1_800_000_000,
  acceptedAt: null,
  revokedAt: null,
  createdAt: 1_790_000_000,
  updatedAt: 1_790_000_000,
};

describe("admin user invitations", () => {
  let container: HTMLDivElement;
  let root: Root;
  let queryClient: QueryClient;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("adds an invitations view with status filtering and server pagination", async () => {
    const list = vi.spyOn(api, "adminUserInvitations")
      .mockResolvedValueOnce({ invitations: [invitation], page: 1, pageSize: 20, totalCount: 21 })
      .mockResolvedValueOnce({ invitations: [invitation], page: 1, pageSize: 20, totalCount: 21 })
      .mockResolvedValueOnce({ invitations: [], page: 2, pageSize: 20, totalCount: 21 });
    renderPage("/admin/users?view=invitations");

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('button[aria-current="page"]')?.textContent).toBe("邀请"));
      await vi.waitFor(() => expect(container.textContent).toContain("person@example.test"));
    });
    expect(list).toHaveBeenCalledWith({ page: 1, pageSize: 20, status: "all" });

    await chooseLuxOptionByValue(container, "邀请状态", "revoked");
    await act(async () => {
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith({ page: 1, pageSize: 20, status: "revoked" }));
      const next = container.querySelector<HTMLButtonElement>('button[aria-label="邀请下一页"]');
      await vi.waitFor(() => expect(next?.disabled).toBe(false));
      next?.click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith({ page: 2, pageSize: 20, status: "revoked" }));
    });
  });

  it("creates from a permission preset and keeps the one-time link out of query and browser storage", async () => {
    const list = vi.spyOn(api, "adminUserInvitations").mockResolvedValue({ invitations: [], page: 1, pageSize: 20, totalCount: 0 });
    const create = vi.spyOn(api, "createAdminUserInvitation").mockResolvedValue({
      invitation,
      invitationUrl: "/register/invitation#token=one-time-secret",
      emailQueued: true,
    });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    renderPage("/admin/users?view=invitations");

    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('form[aria-label="创建邀请"]')).toBeTruthy());
      await vi.waitFor(() => expect(luxCombobox(container, "邀请角色预设")).not.toBeNull());
    });

    await chooseLuxOptionByValue(container, "邀请角色预设", preset.id);
    await vi.waitFor(() => expect(container.textContent).toContain("电影库"));
    act(() => {
      const email = container.querySelector<HTMLInputElement>('input[aria-label="邀请邮箱"]');
      if (email) {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(email, "person@example.test");
        email.dispatchEvent(new Event("input", { bubbles: true }));
        email.dispatchEvent(new Event("change", { bubbles: true }));
      }
    });
    await act(async () => {
      container.querySelector<HTMLFormElement>('form[aria-label="创建邀请"]')?.dispatchEvent(
        new Event("submit", { bubbles: true, cancelable: true }),
      );
      await vi.waitFor(() => expect(create).toHaveBeenCalledWith({ recipientEmail: "person@example.test", presetId: preset.id }));
      await create.mock.results[0]?.value;
    });
    await vi.waitFor(() => expect(container.textContent).toContain("邀请邮件已排队"));
    await vi.waitFor(() => expect(container.querySelector('textarea[aria-label="一次性邀请链接"]')).toBeTruthy());
    await act(async () => {
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
    expect(container.textContent).toContain("邀请邮件已排队");
    expect(container.querySelector<HTMLTextAreaElement>('textarea[aria-label="一次性邀请链接"]')?.value)
      .toBe("https://media.example.test/media/register/invitation#token=one-time-secret");

    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="复制邀请链接"]')?.click();
      await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith("https://media.example.test/media/register/invitation#token=one-time-secret"));
      await writeText.mock.results[0]?.value;
    });
    const cachedData = queryClient.getQueryCache().getAll().map((query) => query.state.data);
    expect(JSON.stringify(cachedData)).not.toContain("one-time-secret");
    expect(localStorage.getItem("lux-invitation-link")).toBeNull();
    expect(sessionStorage.getItem("lux-invitation-link")).toBeNull();

    Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="复制邀请链接"]')?.click();
      await vi.waitFor(() => expect(container.textContent).toContain("无法自动复制，请选中上方链接并手动复制。"));
    });
    const linkField = container.querySelector<HTMLTextAreaElement>('textarea[aria-label="一次性邀请链接"]');
    expect(document.activeElement).toBe(linkField);
    expect(linkField?.selectionStart).toBe(0);
    expect(linkField?.selectionEnd).toBe(linkField?.value.length);
  });

  it("reuses the same idempotency key when retrying a failed resend", async () => {
    vi.spyOn(api, "adminUserInvitations").mockResolvedValue({ invitations: [invitation], page: 1, pageSize: 20, totalCount: 1 });
    const resend = vi.spyOn(api, "resendAdminUserInvitation")
      .mockRejectedValueOnce(new Error("邮件队列暂不可用"))
      .mockResolvedValueOnce({ emailQueued: true });
    renderPage("/admin/users?view=invitations");

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("person@example.test"));
      container.querySelector<HTMLButtonElement>('button[aria-label="重新发送邀请给 person@example.test"]')?.click();
      await vi.waitFor(() => expect(resend).toHaveBeenCalledTimes(1));
    });
    const firstKey = resend.mock.calls[0]?.[1];
    expect(firstKey).toMatch(/^[\da-f-]{36}$/i);
    expect(container.textContent).toContain("邮件队列暂不可用");

    await act(async () => {
      container.querySelector<HTMLButtonElement>('button[aria-label="重新发送邀请给 person@example.test"]')?.click();
      await vi.waitFor(() => expect(resend).toHaveBeenCalledTimes(2));
    });
    expect(resend.mock.calls[1]?.[1]).toBe(firstKey);
    expect(container.textContent).toContain("邀请邮件已重新排队");
  });

  it("confirms before revoking and refreshes the invitation list after success", async () => {
    const list = vi.spyOn(api, "adminUserInvitations").mockResolvedValue({ invitations: [invitation], page: 1, pageSize: 20, totalCount: 1 });
    const revoke = vi.spyOn(api, "revokeAdminUserInvitation").mockResolvedValue({ revoked: true });
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false).mockReturnValue(true);
    renderPage("/admin/users?view=invitations");

    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("person@example.test"));
      const revokeButton = container.querySelector<HTMLButtonElement>('button[aria-label="撤销发往 person@example.test 的邀请"]');
      revokeButton?.click();
      expect(revoke).not.toHaveBeenCalled();
      revokeButton?.click();
      await vi.waitFor(() => expect(revoke).toHaveBeenCalledWith(invitation.id));
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
    });
    expect(confirm).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("邀请已撤销");
  });

  function renderPage(initialEntry = "/admin/users") {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    vi.spyOn(api, "adminUsers").mockResolvedValue({ users: [], page: 1, pageSize: 20, totalCount: 0 });
    vi.spyOn(api, "adminSettings").mockResolvedValue({ userDeletionGraceDays: 7 } as AdminSettings);
    vi.spyOn(api, "adminUserPermissionPresets").mockResolvedValue({ presets: [preset] });
    vi.spyOn(api, "adminLibraries").mockResolvedValue({ libraries: [
      { id: "library-movies", name: "电影库" } as AdminLibrary,
    ] });
    vi.spyOn(api, "adminMailSettings").mockResolvedValue({
      configured: true,
      credentialsConfigured: false,
      settings: {
        host: "smtp.example.test",
        port: 465,
        tlsMode: "SMTPS",
        senderName: "Lux",
        senderEmail: "no-reply@example.test",
        siteUrl: "https://media.example.test/media/",
      },
    });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={[initialEntry]}>
          <AdminUsersPage />
        </MemoryRouter>
      </QueryClientProvider>,
    ));
  }

  function buttonByText(label: string) {
    return Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
      .find((button) => button.textContent?.trim() === label) ?? null;
  }
});
