// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../src/lib/api/client";
import { queryKeys } from "../src/lib/api/query-keys";
import type { AccountPreferences, AuthSession } from "../src/lib/api/types";
import {
  readAccountSettings,
  saveAccountSettings,
  DEFAULT_ACCOUNT_SETTINGS,
} from "../src/features/account/account-settings";
import { ApiError } from "../src/lib/api/client";
import {
  readAutoPlayNextEpisodePreference,
  syncAutoPlayNextEpisodePreference,
  writeAutoPlayNextEpisodePreference,
} from "../src/features/player/player-preferences";
import { useAutoPlayNextPreference } from "../src/features/player/useAutoPlayNextPreference";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const PREFERENCES: AccountPreferences = {
  theme: "dark",
  accentColor: "silver",
  showMediaLibraries: true,
  showContinueWatching: true,
  audioLanguage: "原始音轨",
  subtitleLanguage: "简体中文",
  autoPlayNextEpisode: true,
  libraryOrder: [],
  playedPercent: 95,
  version: 4,
  updatedAt: 1_700_000_000,
};

function session(userId: string): AuthSession {
  return {
    user: {
      id: userId,
      usernameNormalized: userId,
      displayName: userId,
      canManageServer: false,
    },
  };
}

describe("LuxPlayer auto play next preference storage", () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("defaults to enabled and reads the stored account preference", () => {
    expect(DEFAULT_ACCOUNT_SETTINGS.autoPlayNextEpisode).toBe(true);
    expect(readAutoPlayNextEpisodePreference()).toBe(true);

    saveAccountSettings({ ...DEFAULT_ACCOUNT_SETTINGS, autoPlayNextEpisode: false }, "user-1");
    expect(readAutoPlayNextEpisodePreference("user-1")).toBe(false);
    expect(readAutoPlayNextEpisodePreference()).toBe(true);
  });

  it("writes the preference without clobbering the other account settings", () => {
    saveAccountSettings(
      { ...DEFAULT_ACCOUNT_SETTINGS, theme: "light", playedPercent: 80, autoPlayNextEpisode: true },
      "user-1",
    );

    writeAutoPlayNextEpisodePreference(false, "user-1");

    const stored = readAccountSettings("user-1");
    expect(stored.autoPlayNextEpisode).toBe(false);
    expect(stored.theme).toBe("light");
    expect(stored.playedPercent).toBe(80);
  });

  it("patches the existing account preference API with the current version", async () => {
    vi.spyOn(api, "accountPreferences").mockResolvedValue(PREFERENCES);
    const update = vi.spyOn(api, "updateAccountPreferences").mockResolvedValue({
      ...PREFERENCES,
      autoPlayNextEpisode: false,
      version: 5,
    });

    const saved = await syncAutoPlayNextEpisodePreference(false, "user-1");

    expect(api.accountPreferences).toHaveBeenCalledTimes(1);
    expect(update).toHaveBeenCalledWith({
      theme: "dark",
      accentColor: "silver",
      showMediaLibraries: true,
      showContinueWatching: true,
      audioLanguage: "原始音轨",
      subtitleLanguage: "简体中文",
      autoPlayNextEpisode: false,
      libraryOrder: [],
      playedPercent: 95,
      expectedVersion: 4,
    });
    expect(saved?.version).toBe(5);
  });

  it("skips the request when there is no signed-in account or the value already matches", async () => {
    const update = vi.spyOn(api, "updateAccountPreferences");
    vi.spyOn(api, "accountPreferences").mockResolvedValue(PREFERENCES);

    expect(await syncAutoPlayNextEpisodePreference(false, undefined)).toBeNull();
    expect(api.accountPreferences).not.toHaveBeenCalled();

    expect(await syncAutoPlayNextEpisodePreference(true, "user-1")).toEqual(PREFERENCES);
    expect(update).not.toHaveBeenCalled();
  });

  it("keeps the local preference when the account API is unavailable", async () => {
    vi.spyOn(api, "accountPreferences").mockRejectedValue(new Error("offline"));

    expect(await syncAutoPlayNextEpisodePreference(false, "user-1")).toBeNull();
  });

  it("re-reads once and retries when another device bumped the preference version", async () => {
    const conflict = new ApiError("版本冲突", { code: "VERSION_CONFLICT", status: 409 });
    const get = vi.spyOn(api, "accountPreferences")
      .mockResolvedValueOnce({ ...PREFERENCES, version: 4 })
      .mockResolvedValueOnce({ ...PREFERENCES, version: 5 });
    const update = vi.spyOn(api, "updateAccountPreferences")
      .mockRejectedValueOnce(conflict)
      .mockResolvedValueOnce({ ...PREFERENCES, autoPlayNextEpisode: false, version: 6 });

    const saved = await syncAutoPlayNextEpisodePreference(false, "user-1");

    expect(get).toHaveBeenCalledTimes(2);
    expect(update).toHaveBeenCalledTimes(2);
    expect(update).toHaveBeenNthCalledWith(1, expect.objectContaining({ expectedVersion: 4 }));
    expect(update).toHaveBeenNthCalledWith(2, expect.objectContaining({ expectedVersion: 5 }));
    expect(saved?.version).toBe(6);
  });

  it("serializes consecutive toggles instead of racing them", async () => {
    const log: string[] = [];
    let releaseFirstPatch: (() => void) | undefined;
    vi.spyOn(api, "accountPreferences").mockImplementation(async () => {
      log.push("get");
      // The first read still reports the old value; the second one observes the
      // patch that the queued first sync already wrote.
      const patched = log.some((entry) => entry === "patch:false");
      return { ...PREFERENCES, autoPlayNextEpisode: !patched };
    });
    vi.spyOn(api, "updateAccountPreferences").mockImplementation(async (input) => {
      log.push(`patch:${input.autoPlayNextEpisode}`);
      if (input.autoPlayNextEpisode === false) {
        await new Promise<void>((resolve) => {
          releaseFirstPatch = resolve;
        });
      }
      return { ...PREFERENCES, autoPlayNextEpisode: input.autoPlayNextEpisode, version: 5 };
    });

    const first = syncAutoPlayNextEpisodePreference(false, "user-1");
    const second = syncAutoPlayNextEpisodePreference(true, "user-1");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(log).toEqual(["get", "patch:false"]);

    releaseFirstPatch?.();
    await first;
    await second;
    expect(log).toEqual(["get", "patch:false", "get", "patch:true"]);
  });
});

describe("LuxPlayer auto play next preference hook", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  beforeEach(() => {
    window.localStorage.clear();
  });

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
    vi.restoreAllMocks();
  });

  function Probe() {
    const { autoPlayEnabled, setAutoPlayEnabled } = useAutoPlayNextPreference();
    return (
      <button
        type="button"
        data-testid="toggle"
        data-enabled={autoPlayEnabled ? "true" : "false"}
        onClick={() => setAutoPlayEnabled(!autoPlayEnabled)}
      >
        {autoPlayEnabled ? "已开启" : "已关闭"}
      </button>
    );
  }

  it("uses the cached session account and syncs the toggle with the preference API", async () => {
    vi.spyOn(api, "accountPreferences").mockResolvedValue(PREFERENCES);
    vi.spyOn(api, "updateAccountPreferences").mockResolvedValue({
      ...PREFERENCES,
      autoPlayNextEpisode: false,
      version: 5,
    });
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    queryClient.setQueryData(queryKeys.me, session("user-1"));

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => root?.render(
      <QueryClientProvider client={queryClient}>
        <Probe />
      </QueryClientProvider>,
    ));

    const toggle = container.querySelector<HTMLButtonElement>("[data-testid='toggle']");
    expect(toggle?.dataset.enabled).toBe("true");

    await act(async () => toggle?.click());
    expect(toggle?.dataset.enabled).toBe("false");
    expect(readAccountSettings("user-1").autoPlayNextEpisode).toBe(false);

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(api.updateAccountPreferences).toHaveBeenCalledWith(
      expect.objectContaining({ autoPlayNextEpisode: false, expectedVersion: 4 }),
    );
    expect(queryClient.getQueryData<AccountPreferences>(queryKeys.accountPreferences("user-1"))?.version)
      .toBe(5);
  });

  it("uses the cached account preference when local storage has no record", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    queryClient.setQueryData(queryKeys.me, session("user-1"));
    queryClient.setQueryData(queryKeys.accountPreferences("user-1"), {
      ...PREFERENCES,
      autoPlayNextEpisode: false,
      version: 3,
    });
    const get = vi.spyOn(api, "accountPreferences");

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => root?.render(
      <QueryClientProvider client={queryClient}>
        <Probe />
      </QueryClientProvider>,
    ));

    const toggle = container.querySelector<HTMLButtonElement>("[data-testid='toggle']");
    expect(toggle?.dataset.enabled).toBe("false");
    expect(readAccountSettings("user-1").autoPlayNextEpisode).toBe(false);
    expect(get).not.toHaveBeenCalled();
  });

  it("loads the account preference once when local storage is empty", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    queryClient.setQueryData(queryKeys.me, session("user-1"));
    const get = vi.spyOn(api, "accountPreferences").mockResolvedValue({
      ...PREFERENCES,
      autoPlayNextEpisode: false,
      version: 3,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => root?.render(
      <QueryClientProvider client={queryClient}>
        <Probe />
      </QueryClientProvider>,
    ));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const toggle = container.querySelector<HTMLButtonElement>("[data-testid='toggle']");
    expect(toggle?.dataset.enabled).toBe("false");
    expect(get).toHaveBeenCalledTimes(1);
    expect(readAccountSettings("user-1").autoPlayNextEpisode).toBe(false);
    expect(queryClient.getQueryData<AccountPreferences>(queryKeys.accountPreferences("user-1"))?.version)
      .toBe(3);
  });

  it("drops the first server read when the viewer toggled while it was in flight", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    queryClient.setQueryData(queryKeys.me, session("user-1"));
    let releaseRead: ((preferences: AccountPreferences) => void) | undefined;
    const pendingRead = new Promise<AccountPreferences>((resolve) => {
      releaseRead = resolve;
    });
    let reads = 0;
    vi.spyOn(api, "accountPreferences").mockImplementation(async () => {
      reads += 1;
      // 首次读取仍在进行；之后的同步读取看到的是切换后的服务端值。
      return reads === 1 ? pendingRead : { ...PREFERENCES, autoPlayNextEpisode: false, version: 5 };
    });
    vi.spyOn(api, "updateAccountPreferences").mockResolvedValue({
      ...PREFERENCES,
      autoPlayNextEpisode: false,
      version: 5,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => root?.render(
      <QueryClientProvider client={queryClient}>
        <Probe />
      </QueryClientProvider>,
    ));

    const toggle = container.querySelector<HTMLButtonElement>("[data-testid='toggle']");
    expect(toggle?.dataset.enabled).toBe("true");

    // 用户在首次服务端读取返回前手动关闭自动连播。
    await act(async () => toggle?.click());
    expect(toggle?.dataset.enabled).toBe("false");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    // 迟到的首次读取报告旧值（开启），不得覆盖用户的最新选择与缓存版本。
    await act(async () => {
      releaseRead?.({ ...PREFERENCES, autoPlayNextEpisode: true, version: 4 });
      await pendingRead;
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(toggle?.dataset.enabled).toBe("false");
    expect(readAccountSettings("user-1").autoPlayNextEpisode).toBe(false);
    expect(
      queryClient.getQueryData<AccountPreferences>(queryKeys.accountPreferences("user-1"))
        ?.autoPlayNextEpisode,
    ).toBe(false);
  });

  it("keeps a stored local preference instead of loading it again", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    queryClient.setQueryData(queryKeys.me, session("user-1"));
    saveAccountSettings({ ...DEFAULT_ACCOUNT_SETTINGS, autoPlayNextEpisode: true }, "user-1");
    const get = vi.spyOn(api, "accountPreferences").mockResolvedValue({
      ...PREFERENCES,
      autoPlayNextEpisode: false,
      version: 3,
    });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => root?.render(
      <QueryClientProvider client={queryClient}>
        <Probe />
      </QueryClientProvider>,
    ));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(container.querySelector<HTMLButtonElement>("[data-testid='toggle']")?.dataset.enabled).toBe("true");
    expect(get).not.toHaveBeenCalled();
  });

  it("stays usable without a cached session by keeping the preference in local storage", async () => {
    const preferences = vi.spyOn(api, "accountPreferences");
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => root?.render(
      <QueryClientProvider client={queryClient}>
        <Probe />
      </QueryClientProvider>,
    ));

    const toggle = container.querySelector<HTMLButtonElement>("[data-testid='toggle']");
    await act(async () => toggle?.click());

    expect(toggle?.dataset.enabled).toBe("false");
    expect(readAccountSettings().autoPlayNextEpisode).toBe(false);
    expect(preferences).not.toHaveBeenCalled();
  });
});
