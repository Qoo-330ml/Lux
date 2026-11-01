// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";
import { AdminNotificationsPage } from "../src/features/admin/AdminNotificationsPage";
import { api } from "../src/lib/api/client";
import { chooseLuxOptionByValue } from "./lux-select-helpers";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminNotificationsPage", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.spyOn(api, "adminWebhookDestinations").mockResolvedValue({
      destinations: [{
        id: "destination-1",
        name: "本地接收器",
        url: "http://127.0.0.1:8787/hooks",
        payloadFormat: "LUX",
        providerPluginId: "org.lux.webhook",
        providerConfig: {
          url: "http://127.0.0.1:8787/hooks?title={title}&content={content}",
          bodyTemplate: '{"title":"{{title}}","content":"{{content}}"}',
          payloadFormat: "LUX",
        },
        enabled: true,
        allowPrivateNetwork: true,
        eventTypes: ["MEDIA_ADDED"],
        secretConfigured: true,
        createdAt: 1_700_000_000,
        updatedAt: 1_700_000_100,
      }],
      page: 1,
      pageSize: 50,
    });
    vi.spyOn(api, "adminNotificationProviders").mockResolvedValue({
      plugins: [{
        id: "org.lux.webhook",
        name: "Webhook 通知器",
        description: "发送 Lux 事件",
        category: "NOTIFICATION",
        version: "0.1.0",
        runtime: "process",
        capabilities: ["notification.send"],
        status: "READY",
        running: true,
        installed: true,
        enabled: true,
        configured: true,
        available: true,
        configurable: true,
        configFields: [{
          key: "url",
          label: "Webhook URL 模板",
          type: "text",
          required: true,
          sensitive: false,
        }, {
          key: "bodyTemplate",
          label: "Body 模板",
          type: "textarea",
          required: false,
          sensitive: false,
          defaultValue: '{"title":"{{title}}","content":"{{content}}"}',
        }, {
          key: "payloadFormat",
          label: "Payload 格式",
          type: "select",
          required: true,
          sensitive: false,
          defaultValue: "LUX",
          options: [{ value: "LUX", label: "Lux 原生" }, { value: "EMBY", label: "Emby 风格" }],
        }],
        configSource: "PLUGIN_DEFAULT",
      }],
      total: 1,
      page: 1,
      pageSize: 50,
    });
    vi.spyOn(api, "adminWebhookDeliveries").mockResolvedValue({
      deliveries: [{
        id: "delivery-1",
        eventId: "event-1",
        destinationId: "destination-1",
        destinationName: "本地接收器",
        eventType: "MEDIA_ADDED",
        status: "FAILED",
        attemptCount: 8,
        nextAttemptAt: 1_700_000_200,
        lastHttpStatus: 500,
        lastError: "upstream failed",
        title: "扫描完成",
        content: "扫描已完成\n媒体库：library-1",
        deliveredAt: null,
        createdAt: 1_700_000_000,
        updatedAt: 1_700_000_200,
      }],
      page: 1,
      pageSize: 50,
    });
    vi.spyOn(api, "createAdminWebhookDestination").mockResolvedValue({
      destination: {
        id: "destination-2",
        name: "新目标",
        url: "https://example.com/hooks",
        payloadFormat: "EMBY",
        providerPluginId: "org.lux.webhook",
        providerConfig: { payloadFormat: "EMBY" },
        enabled: true,
        allowPrivateNetwork: false,
        eventTypes: [],
        secretConfigured: true,
        createdAt: 1,
        updatedAt: 1,
      },
      secret: "one-time-secret",
    });
    vi.spyOn(api, "retryAdminWebhookDelivery").mockResolvedValue(undefined);
    vi.spyOn(api, "testAdminWebhookDestination").mockResolvedValue({ status: 204 });
    vi.spyOn(api, "rotateAdminWebhookSecret").mockResolvedValue({ secret: "rotated-secret" });
    vi.spyOn(api, "updateAdminWebhookDestination").mockResolvedValue({
      destination: {
        id: "destination-1",
        name: "本地接收器",
        url: "http://127.0.0.1:8787/hooks",
        payloadFormat: "LUX",
        providerPluginId: "org.lux.webhook",
        providerConfig: { payloadFormat: "LUX" },
        enabled: false,
        allowPrivateNetwork: true,
        eventTypes: ["MEDIA_ADDED"],
        secretConfigured: true,
        createdAt: 1_700_000_000,
        updatedAt: 1_700_000_100,
      },
    });
    vi.spyOn(api, "deleteAdminWebhookDestination").mockResolvedValue(undefined);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows configured destinations before provider and delivery data finish loading", async () => {
    vi.mocked(api.adminNotificationProviders).mockReturnValueOnce(new Promise(() => {}));
    vi.mocked(api.adminWebhookDeliveries).mockReturnValueOnce(new Promise(() => {}));
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(MemoryRouter, null, createElement(AdminNotificationsPage)),
      ));
    });
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("本地接收器"));
    });

    expect(container.querySelector(".lux-admin-page-state")).toBeNull();
    expect(container.querySelector(".lux-notification-destination")).toBeTruthy();
  });

  it("keeps the create form collapsed until the new notification action is opened", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(MemoryRouter, null, createElement(AdminNotificationsPage)),
      ));
    });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(container.querySelector('button[aria-label="新建通知"]')).toBeTruthy();
    expect(container.querySelector(".lux-notification-form")).toBeNull();
    expect(container.querySelector('[role="dialog"]')).toBeNull();

    await act(async () => {
      (container.querySelector('button[aria-label="新建通知"]') as HTMLButtonElement).click();
    });

    expect(container.querySelector(".lux-notification-form")).toBeTruthy();
    expect(container.querySelector('[role="dialog"][aria-labelledby="notification-create-title"]')).toBeTruthy();

    await act(async () => {
      (container.querySelector('button[aria-label="关闭新建通知"]') as HTMLButtonElement).click();
    });
    expect(container.querySelector(".lux-notification-form")).toBeNull();
  });

  it("shows destinations, notification content, delivery failures, and retry action", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(MemoryRouter, null, createElement(AdminNotificationsPage)),
      ));
    });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
    await act(async () => {
      (container.querySelector('button[aria-label="新建通知"]') as HTMLButtonElement).click();
    });

    expect(container.textContent).toContain("通知");
    expect(container.textContent).toContain("本地接收器");
    expect(container.textContent).toContain("发送失败");
    expect(container.textContent).toContain("扫描已完成");
    expect(container.textContent).toContain("媒体库：library-1");
    expect(container.querySelector('input[name="event-MEDIA_ADDED"]')).toBeTruthy();
    expect(container.querySelector('[data-name="notification-provider"]')).toBeTruthy();
    expect(container.querySelector('input[name="notification-config-url"]')).toBeTruthy();
    expect(container.querySelector('textarea[name="notification-config-bodyTemplate"]')).toBeTruthy();
    expect(container.querySelector('[data-name="notification-config-payloadFormat"]')).toBeTruthy();
    expect(container.querySelector('input[name="notification-url"]')).toBeNull();
    expect(container.textContent).toContain("通知内容");
    expect(container.textContent).toContain("通知器配置");
    expect(container.querySelector('button[aria-label="重试投递 delivery-1"]')).toBeTruthy();
  });

  it("requires the credential again when the edit form switches notification providers", async () => {
    vi.mocked(api.adminWebhookDestinations).mockResolvedValueOnce({
      destinations: [{
        id: "destination-1",
        name: "本地接收器",
        url: "http://127.0.0.1:8787/hooks",
        payloadFormat: "LUX",
        providerPluginId: "builtin.webhook",
        providerConfig: {},
        enabled: true,
        allowPrivateNetwork: true,
        eventTypes: [],
        secretConfigured: true,
        createdAt: 1_700_000_000,
        updatedAt: 1_700_000_100,
      }],
      page: 1,
      pageSize: 50,
    });
    vi.mocked(api.adminNotificationProviders).mockResolvedValueOnce({
      plugins: [{
        id: "builtin.webhook",
        name: "内置 Webhook",
        description: "Lux 内置签名投递",
        category: "NOTIFICATION",
        version: "1.0.0",
        runtime: "process",
        capabilities: ["notification.send"],
        status: "READY",
        running: true,
        installed: true,
        enabled: true,
        configured: true,
        available: true,
        configurable: false,
        configFields: [],
        configSource: "NONE",
      }, {
        id: "org.lux.bark",
        name: "Bark 通知器",
        description: "推送到 Bark",
        category: "NOTIFICATION",
        version: "0.1.0",
        runtime: "process",
        capabilities: ["notification.send"],
        status: "READY",
        running: true,
        installed: true,
        enabled: true,
        configured: true,
        available: true,
        configurable: true,
        secretField: { key: "secret", label: "设备密钥", type: "password", required: true, sensitive: true },
        configFields: [{
          key: "serverUrl",
          label: "服务地址",
          type: "text",
          required: true,
          sensitive: false,
          defaultValue: "https://api.day.app",
        }],
        configSource: "PLUGIN_DEFAULT",
      }],
      total: 2,
      page: 1,
      pageSize: 50,
    });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(MemoryRouter, null, createElement(AdminNotificationsPage)),
      ));
    });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(container.querySelector('input[type="password"]')).toBeNull();
    const editButton = [...container.querySelectorAll("button")].find((button) => button.textContent?.includes("编辑"));
    expect(editButton).toBeTruthy();
    await act(async () => {
      editButton!.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.querySelector('input[type="password"]')).toBeNull();

    await chooseLuxOptionByValue(container, "编辑通知器", "org.lux.bark");

    const secretInput = container.querySelector<HTMLInputElement>('input[type="password"]');
    expect(secretInput).toBeTruthy();
    expect(secretInput?.hasAttribute("required")).toBe(true);
    expect(secretInput?.getAttribute("placeholder")).toBe("更换通知器后必须重新填写");
    expect(container.textContent).not.toContain("留空表示不修改");
    expect(container.textContent).toContain("更换通知器后旧凭据不会被沿用");
    expect(container.querySelector<HTMLButtonElement>(".lux-notification-edit-form button[type=\"submit\"]")?.disabled).toBe(true);

    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(secretInput, "bark-device-key");
      secretInput!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(container.querySelector<HTMLButtonElement>(".lux-notification-edit-form button[type=\"submit\"]")?.disabled).toBe(false);

    await act(async () => {
      container.querySelector<HTMLFormElement>(".lux-notification-edit-form")!.dispatchEvent(
        new Event("submit", { bubbles: true, cancelable: true }),
      );
    });

    expect(api.updateAdminWebhookDestination).toHaveBeenCalledWith(
      "destination-1",
      expect.objectContaining({
        providerPluginId: "org.lux.bark",
        secret: "bark-device-key",
      }),
    );
  });

  it("asks for the credential a notification provider declares", async () => {
    vi.mocked(api.adminNotificationProviders).mockResolvedValueOnce({
      plugins: [{
        id: "org.lux.bark",
        name: "Bark 通知器",
        description: "推送到 Bark",
        category: "NOTIFICATION",
        version: "0.1.0",
        runtime: "process",
        capabilities: ["notification.send"],
        status: "READY",
        running: true,
        installed: true,
        enabled: true,
        configured: true,
        available: true,
        configurable: true,
        secretField: {
          key: "secret",
          label: "设备密钥",
          type: "password",
          required: true,
          sensitive: true,
          description: "在 Bark App 首页查看",
        },
        configFields: [{
          key: "serverUrl",
          label: "服务地址",
          type: "text",
          required: true,
          sensitive: false,
          defaultValue: "https://api.day.app",
        }],
        configSource: "PLUGIN_DEFAULT",
      }],
      total: 1,
      page: 1,
      pageSize: 50,
    });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(MemoryRouter, null, createElement(AdminNotificationsPage)),
      ));
    });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
    await act(async () => {
      (container.querySelector('button[aria-label="新建通知"]') as HTMLButtonElement).click();
    });

    const secretInput = container.querySelector<HTMLInputElement>('input[name="notification-secret"]');
    expect(secretInput).toBeTruthy();
    expect(secretInput?.getAttribute("type")).toBe("password");
    expect(container.textContent).toContain("设备密钥");
    expect(container.textContent).toContain("在 Bark App 首页查看");
    expect(container.querySelector('[name="notification-config-secret"]')).toBeNull();
    expect(container.querySelector('input[name="notification-config-serverUrl"]')).toBeTruthy();

    const submit = container.querySelector<HTMLButtonElement>('button[type="submit"]');
    expect(submit?.disabled).toBe(true);

    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(secretInput, "bark-device-key");
      secretInput!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false);

    await act(async () => {
      container.querySelector<HTMLFormElement>(".lux-notification-form")!.dispatchEvent(
        new Event("submit", { bubbles: true, cancelable: true }),
      );
    });

    expect(api.createAdminWebhookDestination).toHaveBeenCalledWith(
      expect.objectContaining({
        providerPluginId: "org.lux.bark",
        secret: "bark-device-key",
        providerConfig: { serverUrl: "https://api.day.app" },
      }),
    );
  });
});
