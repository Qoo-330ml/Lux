// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminUserSettingsPage } from "../src/features/admin/AdminUserManagementPage";
import { ApiError, LuxApiClient, api } from "../src/lib/api/client";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const settingsKey = ["admin", "settings", "registration"];

describe("user registration mode settings", () => {
  let container: HTMLDivElement;
  let root: Root;
  let client: QueryClient;

  beforeEach(() => {
    client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    vi.spyOn(api, "adminRegistrationSettings").mockResolvedValue({ registrationMode: "INVITATION_ONLY", version: 0 });
    vi.spyOn(api, "updateAdminRegistrationSettings").mockResolvedValue({ registrationMode: "PUBLIC_EMAIL", version: 1 });
  });

  afterEach(() => {
    act(() => root.unmount());
    client.clear();
    container.remove();
    vi.restoreAllMocks();
    document.cookie = "lux_csrf=;max-age=0;path=/";
  });

  async function render() {
    await act(async () => root.render(<QueryClientProvider client={client}><MemoryRouter initialEntries={["/admin/users/settings?section=registration"]} future={{ v7_startTransition: true, v7_relativeSplatPath: true }}><AdminUserSettingsPage /></MemoryRouter></QueryClientProvider>));
  }
  async function ready() {
    await act(async () => { await vi.waitFor(() => expect(container.querySelector("select")).not.toBeNull()); });
  }
  async function choose(mode = "PUBLIC_EMAIL") {
    await act(async () => {
      const select = container.querySelector("select")!;
      select.value = mode;
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
  }
  function button(text: string) {
    const found = Array.from(container.querySelectorAll("button")).find((element) => element.textContent === text);
    if (!found) throw new Error(`Missing button ${text}`);
    return found;
  }
  async function save() {
    await act(async () => container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })));
  }

  it("shows a loading state before settings arrive", async () => {
    vi.mocked(api.adminRegistrationSettings).mockImplementationOnce(() => new Promise(() => {}));
    await render();
    expect(container.querySelector('[role="status"]')?.textContent).toContain("正在读取注册设置");
    expect(container.querySelector("form")).toBeNull();
  });

  it("loads invitation-only defaults and explains validation, approval and administrator permissions", async () => {
    await render(); await ready();
    expect(container.querySelector("select")?.value).toBe("INVITATION_ONLY");
    expect(button("保存注册模式").disabled).toBe(true);
    expect(container.textContent).toContain("默认采用邀请制");
    expect(container.textContent).toContain("验证邮箱");
    expect(container.textContent).toContain("管理员审批");
    expect(container.textContent).toContain("权限由管理员分配");
    expect(container.textContent).toContain("不会撤销仍有效的已发邀请");
  });

  it("disables pending submission and saves the mode and version in the settings cache", async () => {
    let resolve!: (value: { registrationMode: "PUBLIC_EMAIL"; version: number }) => void;
    vi.mocked(api.updateAdminRegistrationSettings).mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    await render(); await ready(); await choose(); await save(); await save();
    expect(api.updateAdminRegistrationSettings).toHaveBeenCalledTimes(1);
    expect(api.updateAdminRegistrationSettings).toHaveBeenCalledWith({ registrationMode: "PUBLIC_EMAIL", version: 0 });
    expect(container.querySelector("form")?.getAttribute("aria-busy")).toBe("true");
    expect(container.querySelector("select")?.disabled).toBe(true);
    await act(async () => resolve({ registrationMode: "PUBLIC_EMAIL", version: 1 }));
    await act(async () => { await vi.waitFor(() => expect(container.querySelector('[role="status"]')?.textContent).toContain("注册模式已保存")); });
    expect(client.getQueryData(settingsKey)).toEqual({ registrationMode: "PUBLIC_EMAIL", version: 1 });
    expect(button("保存注册模式").disabled).toBe(true);
  });

  it("preserves the draft on a sanitized save error and lets the administrator retry", async () => {
    vi.mocked(api.updateAdminRegistrationSettings).mockRejectedValueOnce(new Error("private SMTP credential details"));
    await render(); await ready(); await choose(); await save();
    await act(async () => { await vi.waitFor(() => expect(container.querySelector('[role="alert"]')).not.toBeNull()); });
    expect(container.textContent).toContain("注册模式保存失败");
    expect(container.textContent).not.toContain("private SMTP");
    expect(container.querySelector("select")?.value).toBe("PUBLIC_EMAIL");
    expect(button("保存注册模式").disabled).toBe(false);
    await save();
    await act(async () => { await vi.waitFor(() => expect(api.updateAdminRegistrationSettings).toHaveBeenCalledTimes(2)); });
    expect(api.updateAdminRegistrationSettings).toHaveBeenLastCalledWith({ registrationMode: "PUBLIC_EMAIL", version: 0 });
  });

  it("shows a load error and supports reloading without a submit form", async () => {
    vi.mocked(api.adminRegistrationSettings).mockRejectedValueOnce(new Error("private details"));
    await render();
    await act(async () => { await vi.waitFor(() => expect(container.querySelector('[role="alert"]')).not.toBeNull()); });
    expect(container.querySelector("form")).toBeNull();
    expect(container.textContent).not.toContain("private details");
    await act(async () => button("重新加载注册设置").click()); await ready();
    expect(container.querySelector("select")?.value).toBe("INVITATION_ONLY");
  });

  it("refreshes a stale version while retaining the selected mode for retry", async () => {
    vi.mocked(api.adminRegistrationSettings).mockResolvedValueOnce({ registrationMode: "INVITATION_ONLY", version: 0 }).mockResolvedValue({ registrationMode: "INVITATION_ONLY", version: 2 });
    vi.mocked(api.updateAdminRegistrationSettings).mockRejectedValueOnce(new ApiError("private conflict", { status: 409, code: "VERSION_CONFLICT" }));
    await render(); await ready(); await choose(); await save();
    await act(async () => { await vi.waitFor(() => expect(client.getQueryData(settingsKey)).toEqual({ registrationMode: "INVITATION_ONLY", version: 2 })); });
    expect(container.querySelector("select")?.value).toBe("PUBLIC_EMAIL");
    expect(container.textContent).toContain("已重新读取最新设置");
    await save();
    expect(api.updateAdminRegistrationSettings).toHaveBeenLastCalledWith({ registrationMode: "PUBLIC_EMAIL", version: 2 });
  });

  it("uses the explicit GET/PATCH client contract with version and CSRF", async () => {
    document.cookie = "lux_csrf=registration-test-csrf;path=/";
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (_url, init) => new Response(JSON.stringify({ registrationMode: init?.method === "PATCH" ? "PUBLIC_EMAIL" : "INVITATION_ONLY", version: init?.method === "PATCH" ? 1 : 0 }), { status: 200 }));
    const apiClient = new LuxApiClient();
    await expect(apiClient.adminRegistrationSettings()).resolves.toEqual({ registrationMode: "INVITATION_ONLY", version: 0 });
    await expect(apiClient.updateAdminRegistrationSettings({ registrationMode: "PUBLIC_EMAIL", version: 0 })).resolves.toEqual({ registrationMode: "PUBLIC_EMAIL", version: 1 });
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/admin/settings/registration");
    const [url, request] = fetchMock.mock.calls[1]!;
    expect(url).toBe("/api/v1/admin/settings/registration");
    expect(request?.method).toBe("PATCH");
    expect(JSON.parse(request?.body as string)).toEqual({ registrationMode: "PUBLIC_EMAIL", version: 0 });
    expect(new Headers(request?.headers).get("X-CSRF-Token")).toBe("registration-test-csrf");
  });
});
