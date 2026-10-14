// @vitest-environment jsdom

import { act } from "react";
import { readFileSync } from "node:fs";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";
import { SetupPage } from "../src/features/auth/SetupPage";
import { api } from "../src/lib/api/client";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const legacyApp = readFileSync("tests/fixtures/legacy-web/app.mjs", "utf8");

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

  it("keeps mixed as the default and submits HOMEVIDEOS when selected for the first library", async () => {
    vi.spyOn(api, "setupDatabaseStatus").mockResolvedValue({
      configured: true,
      currentBackend: "SQLITE",
      restartRequired: false,
    });
    const submit = vi.spyOn(api, "setup").mockResolvedValue({
      user: { id: "admin", usernameNormalized: "admin" },
    });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    await act(async () => {
      root.render(
        <MemoryRouter>
          <QueryClientProvider client={queryClient}>
            <SetupPage />
          </QueryClientProvider>
        </MemoryRouter>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 25));
    });

    const kind = container.querySelector<HTMLSelectElement>("#setup-library-kind");
    expect(kind).not.toBeNull();
    expect(kind?.value).toBe("MIXED");
    expect([...kind!.options].map((option) => option.value)).toContain("HOMEVIDEOS");

    await act(async () => {
      const username = container.querySelector<HTMLInputElement>("#setup-username")!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(username, "admin");
      username.dispatchEvent(new Event("input", { bubbles: true }));
      username.dispatchEvent(new Event("change", { bubbles: true }));
      const password = container.querySelector<HTMLInputElement>("#setup-password")!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(password, "correct password");
      password.dispatchEvent(new Event("input", { bubbles: true }));
      password.dispatchEvent(new Event("change", { bubbles: true }));
      const libraryName = container.querySelector<HTMLInputElement>("#setup-library-name")!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(libraryName, "家庭视频");
      libraryName.dispatchEvent(new Event("input", { bubbles: true }));
      libraryName.dispatchEvent(new Event("change", { bubbles: true }));
      const libraryRoot = container.querySelector<HTMLInputElement>("#setup-library-root")!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(libraryRoot, "/media/home-videos");
      libraryRoot.dispatchEvent(new Event("input", { bubbles: true }));
      libraryRoot.dispatchEvent(new Event("change", { bubbles: true }));
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set?.call(kind, "HOMEVIDEOS");
      kind!.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>("button[type='submit']")!.click();
      await new Promise((resolve) => setTimeout(resolve, 25));
    });

    expect(submit).toHaveBeenCalledWith(expect.objectContaining({
      username: "admin",
      password: "correct password",
      libraryName: "家庭视频",
      libraryKind: "HOMEVIDEOS",
      libraryRoot: "/media/home-videos",
    }));
  });
});

describe("legacy setup form", () => {
  it("offers and submits HOMEVIDEOS as the first library type", () => {
    expect(legacyApp).toContain('<option value=\\"HOMEVIDEOS\\">其他视频</option>');
    expect(legacyApp).toContain('libraryKind: field(form, "libraryKind").value');
  });
});
