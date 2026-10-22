// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode, act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/app/App";
import { ApiError, api } from "../src/lib/api/client";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

describe("invited account creation", () => {
  let container: HTMLDivElement;
  let root: Root;
  let queryClient: QueryClient;
  const token = "a".repeat(43);

  beforeEach(() => {
    vi.spyOn(api, "setupStatus").mockResolvedValue({ initialized: true });
    vi.spyOn(api, "me").mockRejectedValue(new ApiError("需要登录", { status: 401 }));
    vi.spyOn(api, "loginBackground").mockResolvedValue({ source: "STATIC", images: [] });
    vi.spyOn(api, "acceptUserInvitation").mockResolvedValue({ created: true });
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
      root.render(<StrictMode><QueryClientProvider client={queryClient}><BrowserRouter><App /></BrowserRouter></QueryClientProvider></StrictMode>);
    });
  }

  async function fill(id: string, value: string) {
    await act(async () => {
      const input = container.querySelector<HTMLInputElement>(`#${id}`)!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  }

  async function fillAccount() {
    await fill("invitation-username", "invited-user");
    await fill("invitation-display-name", "Invited User");
    await fill("invitation-password", "new-invitation-password");
  }

  function submit() {
    container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  }

  it.each(["", "/", "///"])("clears the fragment and preserves the deployment path with trailing suffix %s", async (suffix) => {
    const storageWrite = vi.spyOn(Storage.prototype, "setItem");
    await render(`/proxy/lux/register/invitation${suffix}#token=${token}`);
    expect(window.location.pathname.replace(/\/+$/, "")).toBe("/proxy/lux/register/invitation");
    expect(window.location.hash).toBe("");
    expect(api.setupStatus).not.toHaveBeenCalled();
    expect(api.me).not.toHaveBeenCalled();
    expect(container.innerHTML).not.toContain(token);
    await fillAccount();
    await act(async () => submit());
    expect(api.acceptUserInvitation).toHaveBeenCalledOnce();
    expect(api.acceptUserInvitation).toHaveBeenCalledWith({
      token,
      username: "invited-user",
      displayName: "Invited User",
      password: "new-invitation-password",
    }, "/proxy/lux");
    expect(container.textContent).toContain("账户创建成功");
    expect(container.querySelector<HTMLAnchorElement>("a")?.getAttribute("href")).toBe("/proxy/lux/");
    expect(container.querySelector("input[type=password]")).toBeNull();
    expect(queryClient.getMutationCache().getAll()).toHaveLength(0);
    expect(queryClient.getQueryCache().getAll()).toHaveLength(0);
    expect(storageWrite).not.toHaveBeenCalled();
  });

  it("shows an invalid-link message without a form when the fragment has no token", async () => {
    await render("/register/invitation#unrelated=value");
    expect(window.location.hash).toBe("");
    expect(container.querySelector("[role=alert]")?.textContent).toContain("邀请链接无效");
    expect(container.querySelector("form")).toBeNull();
    expect(api.acceptUserInvitation).not.toHaveBeenCalled();
  });

  it("blocks duplicate submissions and maps failures without displaying server details", async () => {
    let rejectRequest!: (error: Error) => void;
    vi.mocked(api.acceptUserInvitation).mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectRequest = reject; }));
    await render(`/register/invitation#token=${token}`);
    await fillAccount();
    await act(async () => { submit(); submit(); });
    expect(api.acceptUserInvitation).toHaveBeenCalledTimes(1);
    expect(container.querySelector<HTMLButtonElement>("button[type=submit]")?.disabled).toBe(true);
    expect(container.querySelector("form")?.getAttribute("aria-busy")).toBe("true");
    await act(async () => rejectRequest(new ApiError(`private ${token}`, { status: 409, code: "INVALID_REQUEST" })));
    expect(container.querySelector("[role=alert]")?.textContent).toContain("用户名或邮箱不可用");
    expect(container.innerHTML).not.toContain(token);
    expect(container.innerHTML).not.toContain("private");
    expect(container.querySelector<HTMLInputElement>("#invitation-password")?.value).toBe("");
    expect(window.location.hash).toBe("");
    await fill("invitation-password", "retry-password");
    await act(async () => submit());
    expect(api.acceptUserInvitation).toHaveBeenLastCalledWith(expect.objectContaining({ token, password: "retry-password" }), "");
    expect(container.textContent).toContain("账户创建成功");
  });
});
