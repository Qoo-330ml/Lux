// @vitest-environment jsdom

import { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AccountPage } from "../src/features/account/AccountPage";
import { api, LuxApiClient } from "../src/lib/api/client";
import type { AccountPreferences } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const user = {
  id: "user-1",
  usernameNormalized: "owner",
  displayName: "影院主人",
};

const defaultServerPreferences: AccountPreferences = {
  theme: "dark",
  accentColor: "silver",
  showMediaLibraries: true,
  showContinueWatching: true,
  audioLanguage: "原始音轨",
  subtitleLanguage: "简体中文",
  autoPlayNextEpisode: true,
  libraryOrder: [],
  playedPercent: 95,
  version: 0,
  updatedAt: null,
};

function accountProfile(pendingEmail: string | null = null) {
  return {
    user: {
      ...user,
      email: null,
      emailVerifiedAt: null,
      pendingEmail,
    },
  } as Awaited<ReturnType<typeof api.accountProfile>>;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe("account email verification", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    window.localStorage.clear();
    vi.spyOn(api, "accountProfile").mockResolvedValue(accountProfile());
    vi.spyOn(api, "resendAccountEmailVerification").mockResolvedValue({ accepted: true });
    vi.spyOn(api, "verifyAccountEmail").mockResolvedValue({ verified: true });
    vi.spyOn(api, "accountPreferences").mockResolvedValue({ ...defaultServerPreferences });
    vi.spyOn(api, "updateAccountPreferences").mockImplementation(async (input) => ({
      ...defaultServerPreferences,
      ...input,
      version: input.expectedVersion + 1,
    }));
    vi.spyOn(api, "libraries").mockResolvedValue({ libraries: [] });
    vi.spyOn(api, "userSettings").mockResolvedValue({ playedPercent: 95 });
    vi.spyOn(api, "updateUserSettings").mockResolvedValue({ playedPercent: 95 });
    vi.spyOn(api, "updateLibraryOrder").mockImplementation(async (input) => input);
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    window.localStorage.clear();
    window.history.replaceState(null, "", `${window.location.pathname}${window.location.search}`);
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  async function renderAccount(queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } })) {
    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/account/user-1"]}>
            <AccountPage user={user} />
          </MemoryRouter>
        </QueryClientProvider>,
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    return queryClient;
  }

  async function waitForText(text: string) {
    for (let attempt = 0; attempt < 30; attempt += 1) {
      if (container.textContent?.includes(text)) return;
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 10));
      });
    }
    throw new Error(`页面未显示预期内容：${text}`);
  }

  it("restores the pending email from the server and offers a resend action", async () => {
    vi.mocked(api.accountProfile).mockResolvedValue(accountProfile("pending@example.test"));
    await renderAccount();
    await waitForText("待验证邮箱：pending@example.test");

    expect(container.querySelector<HTMLButtonElement>('[aria-label="重新发送邮箱验证邮件"]')).not.toBeNull();
    expect(container.textContent).not.toContain("邮箱已验证");
  });

  it("removes a verification token from the URL fragment before rendering the account", async () => {
    const token = "unverified-token-value";
    let fragmentWhenRequested: string | undefined;
    vi.mocked(api.verifyAccountEmail).mockImplementation(async (input) => {
      fragmentWhenRequested = window.location.hash;
      expect(input).toEqual({ token });
      return { verified: true };
    });
    window.history.replaceState(null, "", `/account/user-1?from=email#token=${token}`);

    const queryClient = await renderAccount();
    await waitForText("邮箱验证成功。");

    expect(window.location.pathname).toBe("/account/user-1");
    expect(window.location.search).toBe("?from=email");
    expect(window.location.hash).toBe("");
    expect(fragmentWhenRequested).toBe("");
    expect(window.localStorage.getItem("lux.account.emailVerificationToken")).toBeNull();
    const cachedProfile = queryClient.getQueryCache().getAll().map(({ state }) => state.data);
    expect(JSON.stringify(cachedProfile)).not.toContain(token);
  });

  it("shows the verification loading state while the server processes the token", async () => {
    const request = deferred<{ verified: true }>();
    vi.mocked(api.verifyAccountEmail).mockReturnValue(request.promise);
    window.history.replaceState(null, "", "/account/user-1#token=loading-token");

    await renderAccount();
    await waitForText("正在验证邮箱…");

    expect(window.location.hash).toBe("");
    await act(async () => request.resolve({ verified: true }));
    await waitForText("邮箱验证成功。");
  });

  it("shows a pending target after the profile save response", async () => {
    vi.spyOn(api, "updateAccountProfile").mockResolvedValue({
      user: { ...user, email: "old@example.test", emailVerifiedAt: 1, pendingEmail: "new@example.test" },
    });
    await renderAccount();

    const emailInput = container.querySelector<HTMLInputElement>('input[type="email"]');
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(emailInput, "new@example.test");
      emailInput?.dispatchEvent(new Event("input", { bubbles: true }));
      emailInput?.dispatchEvent(new Event("change", { bubbles: true }));
      container.querySelector<HTMLFormElement>(".lux-account-form-grid")?.dispatchEvent(
        new Event("submit", { bubbles: true, cancelable: true }),
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    await waitForText("待验证邮箱：new@example.test");
    expect(emailInput?.value).toBe("new@example.test");
    expect(container.textContent).not.toContain("邮箱已验证");
    expect(container.textContent).toContain("验证邮件已发送");
  });

  it("resends a verification email and reports the result", async () => {
    vi.mocked(api.accountProfile).mockResolvedValue(accountProfile("pending@example.test"));
    await renderAccount();
    await waitForText("待验证邮箱：pending@example.test");

    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="重新发送邮箱验证邮件"]')?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(api.resendAccountEmailVerification).toHaveBeenCalledOnce();
    await waitForText("验证邮件已重新发送，请检查邮箱。");
  });

  it("shows a loading state while a resend request is pending", async () => {
    const request = deferred<{ accepted: true }>();
    vi.mocked(api.resendAccountEmailVerification).mockReturnValue(request.promise);
    vi.mocked(api.accountProfile).mockResolvedValue(accountProfile("pending@example.test"));
    await renderAccount();
    await waitForText("待验证邮箱：pending@example.test");

    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="重新发送邮箱验证邮件"]')?.click();
    });
    await waitForText("正在发送验证邮件…");
    expect(container.querySelector<HTMLButtonElement>('[aria-label="重新发送邮箱验证邮件"]')?.disabled).toBe(true);

    await act(async () => request.resolve({ accepted: true }));
    await waitForText("验证邮件已重新发送，请检查邮箱。");
  });

  it("reports a failed verification after removing the token from the URL", async () => {
    const token = "expired-token-value";
    vi.mocked(api.verifyAccountEmail).mockRejectedValue(new Error("凭证无效或已过期"));
    window.history.replaceState(null, "", `/account/user-1#token=${token}`);

    await renderAccount();
    await waitForText("邮箱验证失败：凭证无效或已过期");

    expect(window.location.hash).toBe("");
    expect(window.localStorage.getItem("lux.account.emailVerificationToken")).toBeNull();
  });

  it("uses the authenticated account verification endpoints and keeps the token in the request body", async () => {
    const client = new LuxApiClient();
    const fetchMock = vi.spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(new Response(JSON.stringify({ verified: true }), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ accepted: true }), { status: 202 }));

    await client.verifyAccountEmail({ token: "secret-token" });
    await client.resendAccountEmailVerification();

    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/v1/auth/profile/email-verification/verify", expect.objectContaining({
      method: "POST",
      body: JSON.stringify({ token: "secret-token" }),
      credentials: "same-origin",
    }));
    expect(fetchMock).toHaveBeenNthCalledWith(2, "/api/v1/auth/profile/email-verification/resend", expect.objectContaining({
      method: "POST",
      credentials: "same-origin",
    }));
    expect(JSON.stringify(fetchMock.mock.calls.map(([url]) => url))).not.toContain("secret-token");
  });
});
