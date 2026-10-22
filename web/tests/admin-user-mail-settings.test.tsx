// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserSettingsPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AdminMailSettingsResponse, MailSettings } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserSettingsPage SMTP settings", () => {
  let container: HTMLDivElement;
  let root: Root | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    root = null;
    container.remove();
    vi.restoreAllMocks();
  });

  it("loads SMTP settings without exposing configured credentials", async () => {
    renderPage(mailSettings({ configured: true, credentialsConfigured: true }));
    await waitForInput("SMTP 主机");

    expect(input("SMTP 主机").value).toBe("smtp.example.test");
    expect(input("SMTP 端口").value).toBe("465");
    expect(input("SMTP TLS 模式").value).toBe("SMTPS");
    expect(input("SMTP 密码").value).toBe("");
    expect(container.textContent).toContain("SMTP 凭据已配置");
    expect(container.textContent).not.toContain("smtp-user");
    expect(container.textContent).not.toContain("private-secret");
  });

  it("saves validated SMTP settings and updates the saved state", async () => {
    const queryClient = renderPage(mailSettings());
    const save = vi.spyOn(api, "updateAdminMailSettings").mockResolvedValue(undefined);
    await waitForInput("SMTP 主机");

    await act(async () => {
      changeInput(input("SMTP 发件人邮箱"), "mail@example.test");
    });
    const saveButton = button("保存 SMTP 设置");
    expect(saveButton.disabled).toBe(false);
    await act(async () => {
      saveButton.click();
      await vi.waitFor(() => expect(save).toHaveBeenCalledWith({
        host: "smtp.example.test",
        port: 465,
        tlsMode: "SMTPS",
        senderName: "Lux",
        senderEmail: "mail@example.test",
        siteUrl: "https://lux.example.test",
      }));
    });
    expect(container.querySelector('[role="status"]')?.textContent).toContain("SMTP 设置已保存");
    expect(queryClient.getQueryData<AdminMailSettingsResponse>(["admin", "settings", "mail"])?.configured).toBe(true);
  });

  it("reports SMTP settings save failures accessibly", async () => {
    renderPage(mailSettings());
    const save = vi.spyOn(api, "updateAdminMailSettings").mockRejectedValue(new Error("SMTP 设置无效"));
    await waitForInput("SMTP 主机");

    await act(async () => {
      changeInput(input("SMTP 发件人邮箱"), "mail@example.test");
      button("保存 SMTP 设置").click();
      await vi.waitFor(() => expect(save).toHaveBeenCalledOnce());
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("SMTP 设置无效"));
    });
    expect(button("保存 SMTP 设置").disabled).toBe(false);
  });

  it("saves a credential pair, clears both inputs, and deletes credentials separately", async () => {
    const queryClient = renderPage(mailSettings());
    const save = vi.spyOn(api, "updateAdminMailCredentials").mockResolvedValue(undefined);
    const remove = vi.spyOn(api, "deleteAdminMailCredentials").mockResolvedValue(undefined);
    await waitForInput("SMTP 用户名");

    await act(async () => {
      changeInput(input("SMTP 用户名"), "smtp-user");
    });
    expect(button("保存 SMTP 凭据").disabled).toBe(true);
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("同时填写");
    await act(async () => {
      changeInput(input("SMTP 密码"), "private-secret");
    });
    await act(async () => {
      button("保存 SMTP 凭据").click();
      await vi.waitFor(() => expect(save).toHaveBeenCalledWith({ username: "smtp-user", password: "private-secret" }));
    });
    expect(input("SMTP 用户名").value).toBe("");
    expect(input("SMTP 密码").value).toBe("");
    expect(container.textContent).toContain("SMTP 凭据已配置");
    expect(queryClient.getQueryData<AdminMailSettingsResponse>(["admin", "settings", "mail"])?.credentialsConfigured).toBe(true);
    expect(queryClient.getMutationCache().getAll().some((mutation) => JSON.stringify(mutation.state.variables).includes("private-secret"))).toBe(false);

    vi.spyOn(window, "confirm").mockReturnValue(true);
    await act(async () => {
      button("删除 SMTP 凭据").click();
      await vi.waitFor(() => expect(remove).toHaveBeenCalledOnce());
    });
    expect(queryClient.getQueryData<AdminMailSettingsResponse>(["admin", "settings", "mail"])?.credentialsConfigured).toBe(false);
  });

  it("checks SMTP connectivity and shows a sanitized error", async () => {
    renderPage(mailSettings({ configured: true }));
    const check = vi.spyOn(api, "testAdminMailConnection").mockResolvedValue({ reachable: true });
    await waitForInput("SMTP 主机");

    await act(async () => {
      button("检查 SMTP 连接").click();
      await vi.waitFor(() => expect(check).toHaveBeenCalledOnce());
    });
    expect(container.textContent).toContain("SMTP 连接成功。");

    check.mockRejectedValueOnce(new Error("SMTP 连接失败"));
    await act(async () => {
      button("检查 SMTP 连接").click();
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("SMTP 连接失败"));
    });
    expect(container.textContent).not.toContain("private-secret");
  });

  function renderPage(response: AdminMailSettingsResponse) {
    vi.spyOn(api, "adminMailSettings").mockResolvedValue(response);
    if (!vi.isMockFunction(api.adminRegistrationSettings)) {
      vi.spyOn(api, "adminRegistrationSettings").mockResolvedValue({ registrationMode: "INVITATION_ONLY", version: 1 });
    }
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root?.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/settings?section=mail"]}>
          <AdminUserSettingsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    ));
    return queryClient;
  }

  async function waitForInput(label: string) {
    await act(async () => {
      await vi.waitFor(() => expect(input(label)).toBeTruthy());
    });
  }

  function input(label: string) {
    const field = container.querySelector<HTMLInputElement | HTMLSelectElement>(`[aria-label="${label}"]`);
    expect(field, `input labeled ${label}`).toBeTruthy();
    return field!;
  }

  function button(label: string) {
    const field = [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.includes(label));
    expect(field, `button containing ${label}`).toBeTruthy();
    return field!;
  }

  function changeInput(field: HTMLInputElement | HTMLSelectElement, value: string) {
    const prototype = field instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, "value")?.set?.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
    field.dispatchEvent(new Event("change", { bubbles: true }));
  }
});

function mailSettings(overrides: Partial<AdminMailSettingsResponse> = {}): AdminMailSettingsResponse {
  const settings: MailSettings = {
    host: "smtp.example.test",
    port: 465,
    tlsMode: "SMTPS",
    senderName: "Lux",
    senderEmail: "noreply@example.test",
    siteUrl: "https://lux.example.test",
  };
  return { configured: true, credentialsConfigured: false, settings, ...overrides };
}
