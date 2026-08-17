// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";
import { AdminNotificationsPage } from "../src/features/admin/AdminNotificationsPage";
import { api } from "../src/lib/api/client";

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
        providerConfig: { payloadFormat: "LUX" },
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

  it("shows destinations, event choices, delivery failures, and retry action", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root.render(createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(MemoryRouter, null, createElement(AdminNotificationsPage)),
      ));
    });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

    expect(container.textContent).toContain("通知");
    expect(container.textContent).toContain("本地接收器");
    expect(container.textContent).toContain("发送失败");
    expect(container.querySelector('input[name="event-MEDIA_ADDED"]')).toBeTruthy();
    expect(container.querySelector('select[name="notification-provider"]')).toBeTruthy();
    expect(container.querySelector('select[name="notification-config-payloadFormat"]')).toBeTruthy();
    expect(container.textContent).toContain("通知内容");
    expect(container.textContent).toContain("通知器配置");
    expect(container.querySelector('button[aria-label="重试投递 delivery-1"]')).toBeTruthy();
  });
});
