// @vitest-environment jsdom

import { act } from "react";
import { readFileSync } from "node:fs";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { SetupPage } from "../src/features/auth/SetupPage";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";
import { chooseLuxOptionByValue, luxOptions } from "./lux-select-helpers";

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

    const kind = container.querySelector<HTMLElement>("#setup-library-kind");
    expect(kind).not.toBeNull();
    expect(kind?.dataset.value).toBe("MIXED");
    expect((await luxOptions(container, kind!)).map((option) => option.value)).toContain("HOMEVIDEOS");

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
      await chooseLuxOptionByValue(container, kind!, "HOMEVIDEOS");
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

function setInputValue(target: HTMLElement, selector: string, value: string) {
  const input = target.querySelector<HTMLInputElement>(selector)!;
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
  input.dispatchEvent(new Event("change", { bubbles: true }));
}

const restartingUser = { id: "admin", usernameNormalized: "admin" };

describe("SetupPage setup-complete restart", () => {
  let container: HTMLDivElement;
  let root: Root;
  let queryClient: QueryClient;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.spyOn(api, "setupDatabaseStatus").mockResolvedValue({
      configured: true,
      currentBackend: "SQLITE",
      restartRequired: false,
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  });

  afterEach(() => {
    act(() => root.unmount());
    queryClient.clear();
    container.remove();
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  // Fake timers keep the 1 s probe cadence and the 10 s fallback deterministic:
  // every assertion below is expressed as "after N seconds of probe ticks".
  async function advance(ms: number) {
    await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
  }

  async function renderSetupPage() {
    await act(async () => {
      root.render(
        <MemoryRouter initialEntries={["/setup"]}>
          <QueryClientProvider client={queryClient}>
            <Routes>
              <Route path="/setup" element={<SetupPage />} />
              <Route path="/login" element={<div>登录页占位</div>} />
            </Routes>
          </QueryClientProvider>
        </MemoryRouter>,
      );
    });
    await advance(10);
  }

  async function submitAdminForm() {
    await act(async () => {
      setInputValue(container, "#setup-username", "admin");
      setInputValue(container, "#setup-password", "correct password");
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>("button[type='submit']")!.click();
    });
    await advance(10);
  }

  it("shows the restart screen instead of caching initialization when the server reports a restart", async () => {
    const setup = vi.spyOn(api, "setup").mockResolvedValue({ user: restartingUser, restarting: true });
    const status = vi.spyOn(api, "setupStatus").mockResolvedValue({ initialized: true });

    await renderSetupPage();
    await submitAdminForm();

    expect(setup).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("正在重启 Lux");
    expect(container.querySelector("[aria-busy='true']")).not.toBeNull();
    expect(queryClient.getQueryData(queryKeys.setup)).toBeUndefined();

    // setup/status answers initialized:true as soon as the administrator row
    // exists, i.e. before the restart, so one successful probe proves nothing.
    await advance(1000);
    expect(status).toHaveBeenCalled();
    expect(queryClient.getQueryData(queryKeys.setup)).toBeUndefined();
    expect(container.textContent).not.toContain("登录页占位");
  });

  it("only treats the service as ready after a failed probe is followed by a successful one", async () => {
    vi.spyOn(api, "setup").mockResolvedValue({ user: restartingUser, restarting: true });
    vi.spyOn(api, "setupStatus")
      .mockResolvedValueOnce({ initialized: true })
      .mockResolvedValueOnce({ initialized: true })
      .mockRejectedValueOnce(new TypeError("Failed to fetch"))
      .mockResolvedValue({ initialized: true });

    await renderSetupPage();
    await submitAdminForm();

    // Two probes answer fine while the old process is still serving.
    await advance(2000);
    expect(queryClient.getQueryData(queryKeys.setup)).toBeUndefined();
    expect(container.textContent).not.toContain("登录页占位");

    // Third probe fails: the port is closed, so the disconnect is observed.
    await advance(1000);
    expect(queryClient.getQueryData(queryKeys.setup)).toBeUndefined();
    expect(container.textContent).toContain("正在重启 Lux");

    // Fourth probe succeeds: the restarted process is serving again.
    await advance(1000);
    expect(queryClient.getQueryData(queryKeys.setup)).toEqual({ initialized: true });
    expect(container.textContent).toContain("登录页占位");
  });

  it("falls back to the login page when no disconnect is observed within ten seconds", async () => {
    vi.spyOn(api, "setup").mockResolvedValue({ user: restartingUser, restarting: true });
    vi.spyOn(api, "setupStatus").mockResolvedValue({ initialized: true });

    await renderSetupPage();
    await submitAdminForm();

    await advance(9000);
    expect(queryClient.getQueryData(queryKeys.setup)).toBeUndefined();
    expect(container.textContent).not.toContain("登录页占位");

    await advance(1000);
    expect(queryClient.getQueryData(queryKeys.setup)).toEqual({ initialized: true });
    expect(container.textContent).toContain("登录页占位");
  });

  it("offers a reload hint when the service has not recovered 60 s after the disconnect", async () => {
    vi.spyOn(api, "setup").mockResolvedValue({ user: restartingUser, restarting: true });
    let probes = 0;
    vi.spyOn(api, "setupStatus").mockImplementation(async () => {
      probes += 1;
      // The process died instead of coming back (docs/SETUP-RESTART-DESIGN.md
      // §4.3: a failing exec exits the process), so keep failing for a minute.
      if (probes < 62) throw new TypeError("Failed to fetch");
      return { initialized: true };
    });

    await renderSetupPage();
    await submitAdminForm();

    await advance(1000);
    expect(container.textContent).toContain("正在重启 Lux");
    expect(container.textContent).not.toContain("重启耗时超出预期");

    // Wall clock reaches 60 s without a single successful probe.
    await advance(59_000);
    expect(container.textContent).toContain("Lux 重启耗时超出预期");
    expect(container.textContent).toContain("请检查 Lux 服务日志");
    expect(queryClient.getQueryData(queryKeys.setup)).toBeUndefined();
    const reload = [...container.querySelectorAll<HTMLButtonElement>("button")]
      .find((candidate) => candidate.textContent?.includes("刷新页面"));
    expect(reload).toBeTruthy();

    // Polling continues behind the notice: recovery still ends up on /login.
    await advance(2000);
    expect(queryClient.getQueryData(queryKeys.setup)).toEqual({ initialized: true });
    expect(container.textContent).toContain("登录页占位");
  });
});

describe("legacy setup form", () => {
  it("offers and submits HOMEVIDEOS as the first library type", () => {
    expect(legacyApp).toContain('<option value=\\"HOMEVIDEOS\\">其他视频</option>');
    expect(legacyApp).toContain('libraryKind: field(form, "libraryKind").value');
  });
});
