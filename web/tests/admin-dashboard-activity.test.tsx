// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { AdminDashboardActivity } from "../src/features/admin/AdminDashboardActivity";
import type { AdminActivityEvent } from "../src/lib/api/types";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminDashboardActivity", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("shows the recorded IP and detected location for a playback event", () => {
    const event: AdminActivityEvent = {
      id: "activity-1",
      userName: "pdz",
      eventType: "PLAYBACK_STARTED",
      targetTitle: "起源",
      remoteIp: "8.8.8.8",
      remoteIpLocation: {
        location: "美国 · 加利福尼亚州 · 山景城",
        district: "Santa Clara",
        street: "Amphitheatre Parkway",
        isp: "Google",
      },
      createdAt: 1_700_000_000,
    };

    act(() => {
      root.render(
        <MemoryRouter>
          <AdminDashboardActivity events={[event]} />
        </MemoryRouter>,
      );
    });

    const row = container.querySelector(".lux-admin-activity-row");
    expect(row?.querySelector('[aria-label="IP 地址"]')?.textContent).toContain("8.8.8.8");
    const location = row?.querySelector('[aria-label="IP 归属地"]');
    expect(location?.textContent).toContain("美国 · 加利福尼亚州 · 山景城");
    expect(location?.textContent).toContain("Santa Clara · Amphitheatre Parkway · Google");
  });

  it("keeps the location field hidden when an IP has not been resolved", () => {
    const event: AdminActivityEvent = {
      id: "activity-2",
      userName: "pdz",
      eventType: "PLAYBACK_PAUSED",
      remoteIp: "192.168.1.20",
      remoteIpLocation: null,
      createdAt: 1_700_000_000,
    };

    act(() => {
      root.render(
        <MemoryRouter>
          <AdminDashboardActivity events={[event]} />
        </MemoryRouter>,
      );
    });

    const row = container.querySelector(".lux-admin-activity-row");
    expect(row?.querySelector('[aria-label="IP 地址"]')?.textContent).toContain("192.168.1.20");
    expect(row?.querySelector('[aria-label="IP 归属地"]')).toBeNull();
  });
});
