// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SetupPage } from "../src/features/auth/SetupPage";
import { api } from "../src/lib/api/client";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

describe("SetupPage database restart", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.spyOn(api, "setupDatabaseStatus").mockResolvedValue({
      configured: true,
      backend: "POSTGRESQL",
      currentBackend: "SQLITE",
      restartRequired: true,
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("shows a restart button and waits for the service after clicking it", async () => {
    const restart = vi.spyOn(api, "restartSetupDatabase").mockResolvedValue({ restarting: true });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <SetupPage />
        </QueryClientProvider>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 25));
    });

    const button = [...container.querySelectorAll<HTMLButtonElement>("button")]
      .find((candidate) => candidate.textContent?.includes("重启 Lux"));
    expect(button).toBeTruthy();

    await act(async () => {
      button?.click();
      await new Promise((resolve) => setTimeout(resolve, 25));
    });

    expect(restart).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("正在重启 Lux");
    expect(container.querySelector("[aria-busy='true']")).not.toBeNull();
  });
});
