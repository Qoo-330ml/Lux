// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AdminUserSettingsPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AdminSettings } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("AdminUserSettingsPage deletion retention", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("loads the configured value and saves a valid retention period", async () => {
    const queryClient = renderPage(30);
    const update = vi.spyOn(api, "updateAdminSettings").mockResolvedValue(settings(14));
    await waitForInput();

    const input = findInput();
    expect(input.value).toBe("30");
    expect(container.textContent).toContain("只影响之后移入回收站的账号");
    expect(container.textContent).toContain("活动记录保留将在模块七接入");

    await act(async () => {
      changeInput(input, "14");
      findButton("保存保留期限").click();
      await vi.waitFor(() => expect(update).toHaveBeenCalledWith({ userDeletionGraceDays: 14 }));
    });

    expect(container.querySelector('[role="status"]')?.textContent).toBe("删除宽限期已保存。");
    expect(queryClient.getQueryData<AdminSettings>(["admin", "settings"])?.userDeletionGraceDays).toBe(14);
  });

  it("defaults to seven days and prevents values outside the supported range", async () => {
    renderPage(undefined);
    const update = vi.spyOn(api, "updateAdminSettings").mockResolvedValue(settings(7));
    await waitForInput();

    const input = findInput();
    expect(input.value).toBe("7");
    await act(async () => changeInput(input, "0"));
    expect(findButton("保存保留期限").disabled).toBe(true);
    expect(update).not.toHaveBeenCalled();
  });

  it("keeps the form available and reports a save failure", async () => {
    renderPage(20);
    const update = vi.spyOn(api, "updateAdminSettings").mockRejectedValue(new Error("网络错误"));
    await waitForInput();

    await act(async () => {
      changeInput(findInput(), "21");
      findButton("保存保留期限").click();
      await vi.waitFor(() => expect(update).toHaveBeenCalled());
      await vi.waitFor(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("网络错误"));
    });
    expect(findInput().disabled).toBe(false);
  });

  function renderPage(userDeletionGraceDays: number | undefined) {
    vi.spyOn(api, "adminSettings").mockResolvedValue(settings(userDeletionGraceDays));
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    act(() => root.render(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter initialEntries={["/admin/users/settings?section=retention"]}>
          <AdminUserSettingsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    ));
    return queryClient;
  }

  async function waitForInput() {
    await act(async () => {
      await vi.waitFor(() => expect(container.querySelector('input[aria-label="删除账号保留天数"]')).toBeTruthy());
    });
  }

  function findInput() {
    const input = container.querySelector<HTMLInputElement>('input[aria-label="删除账号保留天数"]');
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

function settings(userDeletionGraceDays?: number): AdminSettings {
  return { userDeletionGraceDays } as AdminSettings;
}
