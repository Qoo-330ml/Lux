// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminUserSettingsPage } from "../src/features/admin/AdminUserManagementPage";
import { api } from "../src/lib/api/client";
import type { AdminUserAccessBlacklistRule, AdminUserAccessBlacklistRulesPage } from "../src/lib/api/types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const ipRule: AdminUserAccessBlacklistRule = {
  id: "rule-ip-1",
  userId: null,
  ruleType: "IP_CIDR",
  matchValue: "192.0.2.0/24",
  isEnabled: false,
  createdBy: "admin-1",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
};

function page(rules: AdminUserAccessBlacklistRule[] = [], pageNumber = 1, totalCount = rules.length): AdminUserAccessBlacklistRulesPage {
  return { rules, page: pageNumber, pageSize: 20, totalCount };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

describe("global user access blacklist settings", () => {
  let container: HTMLDivElement;
  let root: Root;
  let queryClient: QueryClient;

  beforeEach(() => {
    queryClient = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    vi.spyOn(api, "adminGlobalAccessBlacklistRules").mockResolvedValue(page());
    vi.spyOn(api, "createAdminGlobalAccessBlacklistRule").mockImplementation(async (input) => ({
      ...ipRule,
      id: "rule-created",
      ruleType: input.ruleType,
      matchValue: input.ruleType === "CLIENT" ? "infuse player" : input.matchValue,
      isEnabled: input.isEnabled ?? true,
    }));
    vi.spyOn(api, "setAdminGlobalAccessBlacklistRuleEnabled").mockImplementation(async (ruleId, isEnabled) => ({ id: ruleId, isEnabled }));
    vi.spyOn(api, "deleteAdminGlobalAccessBlacklistRule").mockResolvedValue();
    vi.spyOn(api, "adminUserAccessBlacklistRules").mockResolvedValue(page());
    vi.spyOn(api, "createAdminUserAccessBlacklistRule").mockResolvedValue(ipRule);
    vi.spyOn(api, "setAdminUserAccessBlacklistRuleEnabled").mockResolvedValue({ id: ipRule.id, isEnabled: true });
    vi.spyOn(api, "deleteAdminUserAccessBlacklistRule").mockResolvedValue();
    vi.spyOn(window, "confirm").mockReturnValue(true);
  });

  afterEach(() => {
    act(() => root.unmount());
    queryClient.clear();
    container.remove();
    vi.restoreAllMocks();
    document.cookie = "lux_csrf=;max-age=0;path=/";
  });

  async function render() {
    await act(async () => {
      root.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter initialEntries={["/admin/users/settings?section=blocklist"]}>
            <AdminUserSettingsPage />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
  }

  async function waitForText(text: string) {
    await act(async () => {
      await vi.waitFor(() => expect(container.textContent).toContain(text));
    });
  }

  function button(label: string) {
    const found = Array.from(container.querySelectorAll<HTMLButtonElement>("button"))
      .find((candidate) => candidate.textContent?.trim() === label || candidate.getAttribute("aria-label") === label);
    if (!found) throw new Error(`Missing button: ${label}`);
    return found;
  }

  function setInput(input: HTMLInputElement | HTMLSelectElement, value: string) {
    const prototype = input instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, "value")?.set?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }

  async function chooseRuleType(ruleType: string) {
    await act(async () => setInput(container.querySelector<HTMLSelectElement>('[aria-label="规则类型"]')!, ruleType));
  }

  async function setRuleValue(value: string) {
    await act(async () => setInput(container.querySelector<HTMLInputElement>('[aria-label="匹配值"]')!, value));
  }

  async function submitRule() {
    await act(async () => {
      container.querySelector<HTMLFormElement>("form")?.dispatchEvent(
        new Event("submit", { bubbles: true, cancelable: true }),
      );
    });
  }

  it("shows the impact warning, trusted IP source, and a paginated empty state", async () => {
    await render();
    await waitForText("当前没有全站规则");

    expect(api.adminGlobalAccessBlacklistRules).toHaveBeenCalledWith(1, 20);
    expect(container.textContent).toContain("阻止登录、撤销或拒绝已有会话，并阻止播放授权");
    expect(container.textContent).toContain("连接 peer 或已配置的可信代理链");
    expect(container.textContent).toContain("所有用户");
    expect(api.adminUserAccessBlacklistRules).not.toHaveBeenCalled();
  });

  it("shows a loading state while the global page is being fetched", async () => {
    const request = deferred<AdminUserAccessBlacklistRulesPage>();
    vi.mocked(api.adminGlobalAccessBlacklistRules).mockReturnValue(request.promise);
    await render();

    expect(container.textContent).toContain("正在读取全站规则");
    await act(async () => request.resolve(page([ipRule])));
    await waitForText("192.0.2.0/24");
  });

  it("renders rule type, normalized value, enabled state, and bounded pagination", async () => {
    vi.mocked(api.adminGlobalAccessBlacklistRules).mockResolvedValue(page([ipRule], 1, 21));
    await render();
    await waitForText("192.0.2.0/24");

    expect(container.textContent).toContain("IP/CIDR");
    expect(container.textContent).toContain("已停用");
    expect(button("下一页").disabled).toBe(false);
    await act(async () => button("下一页").click());

    expect(api.adminGlobalAccessBlacklistRules).toHaveBeenLastCalledWith(2, 20);
    expect(api.adminUserAccessBlacklistRules).not.toHaveBeenCalled();
  });

  it("validates against server limits and normalizes client names before submitting", async () => {
    await render();
    await waitForText("当前没有全站规则");
    await chooseRuleType("IP_CIDR");
    await setRuleValue("192.0.2.1/33");
    await submitRule();
    expect(container.textContent).toContain("IP 地址或前缀长度无效");
    expect(api.createAdminGlobalAccessBlacklistRule).not.toHaveBeenCalled();

    await chooseRuleType("CLIENT");
    await setRuleValue("InFUSE\u0085 Player");
    await submitRule();
    expect(container.textContent).toContain("匹配值不能包含控制字符");
    expect(api.createAdminGlobalAccessBlacklistRule).not.toHaveBeenCalled();

    await setRuleValue("  InFUSE   Player  ");
    await submitRule();

    expect(window.confirm).toHaveBeenCalledWith(expect.stringContaining("阻止登录、撤销或拒绝已有会话，并阻止播放授权"));
    expect(api.createAdminGlobalAccessBlacklistRule).toHaveBeenCalledWith({
      ruleType: "CLIENT",
      matchValue: "infuse player",
      isEnabled: true,
    });
    await waitForText("规则已创建");
  });

  it("requires confirmation before enabling a rule and refreshes the global list", async () => {
    vi.mocked(api.adminGlobalAccessBlacklistRules).mockResolvedValue(page([ipRule]));
    await render();
    await waitForText("192.0.2.0/24");

    await act(async () => button("启用规则 rule-ip-1").click());

    expect(window.confirm).toHaveBeenCalledWith(expect.stringContaining("已有会话，并阻止播放授权"));
    expect(api.setAdminGlobalAccessBlacklistRuleEnabled).toHaveBeenCalledWith("rule-ip-1", true);
    await waitForText("规则已启用");
    expect(api.adminGlobalAccessBlacklistRules).toHaveBeenCalledTimes(2);
    expect(api.setAdminUserAccessBlacklistRuleEnabled).not.toHaveBeenCalled();
  });

  it("asks before deletion and handles a cancelled confirmation", async () => {
    vi.mocked(api.adminGlobalAccessBlacklistRules).mockResolvedValue(page([ipRule]));
    await render();
    await waitForText("192.0.2.0/24");
    vi.mocked(window.confirm).mockReturnValueOnce(false);
    await act(async () => button("删除规则 rule-ip-1").click());
    expect(api.deleteAdminGlobalAccessBlacklistRule).not.toHaveBeenCalled();

    vi.mocked(window.confirm).mockReturnValue(true);
    await act(async () => button("删除规则 rule-ip-1").click());
    expect(window.confirm).toHaveBeenLastCalledWith(expect.stringContaining("确认删除"));
    expect(api.deleteAdminGlobalAccessBlacklistRule).toHaveBeenCalledWith("rule-ip-1");
  });

  it("shows a retry action after list errors without exposing backend details", async () => {
    vi.mocked(api.adminGlobalAccessBlacklistRules)
      .mockRejectedValueOnce(new Error("internal connection string"))
      .mockResolvedValueOnce(page());
    await render();
    await waitForText("读取全站规则失败");

    expect(container.textContent).not.toContain("internal connection string");
    await act(async () => button("重新加载").click());
    await waitForText("当前没有全站规则");
    expect(api.adminGlobalAccessBlacklistRules).toHaveBeenCalledTimes(2);
  });
});
