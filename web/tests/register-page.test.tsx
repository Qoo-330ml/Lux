// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode, act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/app/App";
import { ApiError, api } from "../src/lib/api/client";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

describe("public user registration", () => {
  let container: HTMLDivElement;
  let root: Root;
  let queryClient: QueryClient;
  const token = "a".repeat(43);

  beforeEach(() => {
    vi.spyOn(api, "setupStatus").mockResolvedValue({ initialized: true });
    vi.spyOn(api, "me").mockRejectedValue(new ApiError("需要登录", { status: 401 }));
    vi.spyOn(api, "loginBackground").mockResolvedValue({ source: "STATIC", images: [] });
    vi.spyOn(api, "publicRegistrationSettings").mockResolvedValue({ registrationMode: "INVITATION_ONLY" });
    vi.spyOn(api, "submitPublicUserRegistration").mockResolvedValue({
      accepted: true,
      message: "若申请符合条件，我们会发送邮件",
    });
    vi.spyOn(api, "verifyPublicUserRegistrationEmail").mockResolvedValue({
      verified: true,
      status: "PENDING_APPROVAL",
    });
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    queryClient.clear();
    container.remove();
    window.history.replaceState({}, "", "/");
    vi.restoreAllMocks();
  });

  async function render(path: string) {
    window.history.replaceState({}, "", path);
    await act(async () => {
      root.render(
        <StrictMode>
          <QueryClientProvider client={queryClient}>
            <BrowserRouter><App /></BrowserRouter>
          </QueryClientProvider>
        </StrictMode>,
      );
    });
    await act(async () => new Promise((resolve) => setTimeout(resolve, 0)));
  }

  async function fill(id: string, value: string) {
    await act(async () => {
      const input = container.querySelector<HTMLInputElement>(`#${id}`)!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  }

  function submit() {
    container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  }

  it("does not show a public form when registration is invitation-only", async () => {
    await render("/register");

    expect(container.textContent).toContain("当前仅接受管理员邀请");
    expect(container.querySelector("form")).toBeNull();
    expect(api.publicRegistrationSettings).toHaveBeenCalledWith("");
    expect(api.submitPublicUserRegistration).not.toHaveBeenCalled();
    expect(api.setupStatus).not.toHaveBeenCalled();
    expect(api.me).not.toHaveBeenCalled();
  });

  it("submits public applications anonymously and shows the approval boundary", async () => {
    vi.mocked(api.publicRegistrationSettings).mockResolvedValue({ registrationMode: "PUBLIC_EMAIL" });
    const storageWrite = vi.spyOn(Storage.prototype, "setItem");
    await render("/proxy/lux/register/");

    await fill("registration-email", "reader@example.test");
    await fill("registration-username", "reader");
    await fill("registration-display-name", "Media Reader");
    await fill("registration-password", "registration-password");
    await act(async () => submit());

    expect(api.submitPublicUserRegistration).toHaveBeenCalledOnce();
    expect(api.submitPublicUserRegistration).toHaveBeenCalledWith({
      email: "reader@example.test",
      username: "reader",
      displayName: "Media Reader",
      password: "registration-password",
    }, "/proxy/lux");
    expect(container.textContent).toContain("请前往邮箱验证");
    expect(container.textContent).toContain("仍需管理员批准");
    expect(container.querySelector("input[type=password]")).toBeNull();
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
    expect(storageWrite).not.toHaveBeenCalled();
  });

  it("clears the email token before verification and reports pending approval", async () => {
    const storageWrite = vi.spyOn(Storage.prototype, "setItem");
    vi.mocked(api.verifyPublicUserRegistrationEmail).mockImplementation(async (input, sitePath) => {
      expect(window.location.hash).toBe("");
      return { verified: true, status: "PENDING_APPROVAL" };
    });
    await render(`/proxy/lux/register/verify-email#token=${token}`);

    expect(window.location.pathname).toBe("/proxy/lux/register/verify-email");
    expect(window.location.hash).toBe("");
    expect(api.publicRegistrationSettings).not.toHaveBeenCalled();
    expect(api.verifyPublicUserRegistrationEmail).toHaveBeenCalledOnce();
    expect(api.verifyPublicUserRegistrationEmail).toHaveBeenCalledWith({ token }, "/proxy/lux");
    expect(container.textContent).toContain("邮箱验证成功，等待管理员批准");
    expect(container.innerHTML).not.toContain(token);
    expect(queryClient.getQueryCache().getAll().map((query) => JSON.stringify(query.state.data)).join("\n"))
      .not.toContain(token);
    expect(storageWrite).not.toHaveBeenCalled();
  });

  it("rejects malformed verification fragments without sending a token", async () => {
    await render("/register/verify-email#token=bad%2Ftoken");

    expect(window.location.hash).toBe("");
    expect(container.querySelector("[role=alert]")?.textContent).toContain("验证链接无效或已失效");
    expect(api.verifyPublicUserRegistrationEmail).not.toHaveBeenCalled();
  });

  it("keeps registration failures generic and available for retry", async () => {
    vi.mocked(api.publicRegistrationSettings).mockResolvedValue({ registrationMode: "PUBLIC_EMAIL" });
    vi.mocked(api.submitPublicUserRegistration).mockRejectedValueOnce(
      new ApiError("private account detail", { status: 429, code: "RATE_LIMITED" }),
    );
    await render("/register");
    await fill("registration-email", "reader@example.test");
    await fill("registration-username", "reader");
    await fill("registration-password", "registration-password");
    await act(async () => submit());

    expect(container.querySelector("[role=alert]")?.textContent).toContain("请求过于频繁");
    expect(container.innerHTML).not.toContain("private account detail");
    expect(container.querySelector("form")).not.toBeNull();
    expect(container.querySelector<HTMLInputElement>("#registration-password")?.value).toBe("");
  });
});
