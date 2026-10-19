// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserDetailPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AdminUser, AdminUserDevicePage } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserDetailPage device inventory", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows a loading state until the device inventory is available", async () => {
    let resolveDevices: ((page: AdminUserDevicePage) => void) | undefined;
    vi.spyOn(api, "adminUserDevices").mockImplementation(() => new Promise((resolve) => {
      resolveDevices = resolve;
    }));
    renderDevices();
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain("正在读取设备清单…"));
    });

    await act(async () => {
      resolveDevices?.({ devices: [makeDevice("device-1")], page: 1, pageSize: 20, totalCount: 1 });
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-device-table")).toBeTruthy());
    });
  });

  it("shows device and client summaries without exposing credentials", async () => {
    const devices: AdminUserDevicePage = {
      devices: [{
        deviceId: "browser-device-1",
        deviceName: "Office Mac",
        deviceType: "Browser",
        clientName: "Lux Web",
        clientVersion: "1.2.3",
        firstSeenAt: 1_700_000_000,
        lastSeenAt: 1_700_000_100,
        lastRemoteIp: "192.0.2.25",
      }],
      page: 1,
      pageSize: 20,
      totalCount: 1,
    };
    const list = vi.spyOn(api, "adminUserDevices").mockResolvedValue(devices);
    renderDevices();
    await waitForDeviceTable();

    expect(list).toHaveBeenCalledWith("user-1", 1, 20);
    expect(container.textContent).toContain("Office Mac");
    expect(container.textContent).toContain("browser-device-1");
    expect(container.textContent).toContain("Lux Web");
    expect(container.textContent).toContain("1.2.3");
    expect(container.textContent).toContain("Browser");
    expect(container.textContent).toContain("192.0.2.25");
    expect(container.textContent).not.toContain("session-token");
    expect(container.textContent).not.toContain("access-token");
  });

  it("paginates the device inventory", async () => {
    const firstPage: AdminUserDevicePage = {
      devices: Array.from({ length: 20 }, (_, index) => makeDevice(`device-${index + 1}`)),
      page: 1,
      pageSize: 20,
      totalCount: 21,
    };
    const secondPage: AdminUserDevicePage = {
      devices: [makeDevice("device-21")],
      page: 2,
      pageSize: 20,
      totalCount: 21,
    };
    const list = vi.spyOn(api, "adminUserDevices").mockImplementation(async (_userId, page = 1) => (
      page === 1 ? firstPage : secondPage
    ));
    renderDevices();
    await waitForDeviceTable();
    expect(container.textContent).toContain("第 1 / 2 页");
    expect(container.querySelectorAll(".lux-admin-user-device-table tbody tr")).toHaveLength(20);
    const devicePanel = container.querySelector<HTMLElement>(".lux-admin-user-devices")!;
    const [previousPage, nextPage] = devicePanel.querySelectorAll<HTMLButtonElement>("nav[aria-label='设备清单分页'] button");
    expect(previousPage.disabled).toBe(true);
    expect(nextPage.disabled).toBe(false);

    await act(async () => {
      findButton("下一页").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledWith("user-1", 2, 20));
      await vi.waitFor(() => expect(container.textContent).toContain("第 2 / 2 页"));
    });
    expect(container.querySelectorAll(".lux-admin-user-device-table tbody tr")).toHaveLength(1);
    expect(container.textContent).toContain("device-21");
    await vi.waitFor(() => expect(container.querySelector<HTMLButtonElement>(".lux-admin-user-devices nav[aria-label='设备清单分页'] button")?.disabled).toBe(false));
    const refreshedButtons = container.querySelectorAll<HTMLButtonElement>(".lux-admin-user-devices nav[aria-label='设备清单分页'] button");
    expect(refreshedButtons[0].disabled).toBe(false);
    expect(refreshedButtons[1].disabled).toBe(true);
  });

  it("allows retry after a read error and shows an empty state", async () => {
    const list = vi.spyOn(api, "adminUserDevices")
      .mockRejectedValueOnce(new Error("temporary device failure"))
      .mockResolvedValueOnce({ devices: [], page: 1, pageSize: 20, totalCount: 0 });
    renderDevices();
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent)
        .toContain("temporary device failure"));
    });

    await act(async () => {
      findButton("重试").click();
      await vi.waitFor(() => expect(list).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(container.textContent).toContain("尚无已记录的设备。"));
    });
  });

  function renderDevices() {
    vi.spyOn(api, "adminUser").mockResolvedValue({ user: makeUser() });
    vi.spyOn(api, "adminUserWebSessions").mockResolvedValue({ sessions: [], page: 1, pageSize: 20, totalCount: 0 });
    vi.spyOn(api, "adminUserEmbySessions").mockResolvedValue({ sessions: [], page: 1, pageSize: 20, totalCount: 0 });
    vi.spyOn(api, "adminUserPlaybackSessions").mockResolvedValue({ sessions: [], page: 1, pageSize: 20, totalCount: 0 });
    if (!vi.isMockFunction(api.adminUserDevices)) {
      vi.spyOn(api, "adminUserDevices").mockResolvedValue({ devices: [], page: 1, pageSize: 20, totalCount: 0 });
    }
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/user-1?section=devices"]}>
          <Routes>
            <Route path="/admin/users/:userId" element={<AdminUserDetailPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ));
  }

  async function waitForDeviceTable() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector(".lux-admin-user-device-table")).toBeTruthy());
    });
  }

  function findButton(label: string) {
    const button = [...container.querySelectorAll<HTMLButtonElement>("button")]
      .find((item) => item.textContent?.includes(label));
    expect(button, `button containing ${label}`).toBeTruthy();
    return button!;
  }
});

function makeUser(): AdminUser {
  return {
    id: "user-1",
    usernameNormalized: "viewer",
    displayName: "观众",
    isDisabled: false,
    isAdmin: false,
  };
}

function makeDevice(deviceId: string) {
  return {
    deviceId,
    deviceName: null,
    deviceType: "Browser",
    clientName: "Lux Web",
    clientVersion: null,
    firstSeenAt: 1_700_000_000,
    lastSeenAt: 1_700_000_100,
    lastRemoteIp: null,
  };
}
