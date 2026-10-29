import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { FormEvent } from "react";
import { useState } from "react";
import { ApiError, api } from "../../lib/api/client";
import type { AdminUserAccessBlacklistRule, AdminUserAccessBlacklistRuleType } from "../../lib/api/types";
import { LuxButton, LuxSelectField, LuxSwitch, LuxTextField, useLuxConfirm } from "../../components/ui";

const PAGE_SIZE = 20;
const globalRulesQueryKey = ["admin", "global-access-blacklist"] as const;
const controlCharacterPattern = /[\u0000-\u001f\u007f-\u009f]/u;

type RuleFormNotice = { kind: "success" | "error"; message: string };

export function AdminUserGlobalAccessRulesPanel() {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [ruleType, setRuleType] = useState<AdminUserAccessBlacklistRuleType>("DEVICE");
  const [matchValue, setMatchValue] = useState("");
  const [createEnabled, setCreateEnabled] = useState(true);
  const [validationError, setValidationError] = useState("");
  const [notice, setNotice] = useState<RuleFormNotice | null>(null);
  const rules = useQuery({
    queryKey: [...globalRulesQueryKey, page],
    queryFn: () => api.adminGlobalAccessBlacklistRules(page, PAGE_SIZE),
    retry: false,
  });

  const createRule = useMutation({
    mutationFn: (input: { ruleType: AdminUserAccessBlacklistRuleType; matchValue: string; isEnabled: boolean }) =>
      api.createAdminGlobalAccessBlacklistRule(input),
    onSuccess: (created) => {
      setMatchValue("");
      setRuleType("DEVICE");
      setCreateEnabled(true);
      setValidationError("");
      setNotice({ kind: "success", message: `规则已创建，服务器匹配值：${created.matchValue}` });
      setPage(1);
      void queryClient.invalidateQueries({ queryKey: globalRulesQueryKey });
    },
    onError: (error) => setNotice({
      kind: "error",
      message: error instanceof ApiError && error.status === 409
        ? "相同范围和匹配值的规则已存在，请检查列表。"
        : "规则创建失败，请核对输入或稍后重试。",
    }),
  });

  const setRuleEnabled = useMutation({
    mutationFn: ({ ruleId, isEnabled }: { ruleId: string; isEnabled: boolean }) =>
      api.setAdminGlobalAccessBlacklistRuleEnabled(ruleId, isEnabled),
    onSuccess: ({ isEnabled }) => {
      setNotice({ kind: "success", message: isEnabled ? "规则已启用。" : "规则已停用。" });
      void queryClient.invalidateQueries({ queryKey: globalRulesQueryKey });
    },
    onError: () => setNotice({ kind: "error", message: "规则状态更新失败，请重试。" }),
  });

  const deleteRule = useMutation({
    mutationFn: (ruleId: string) => api.deleteAdminGlobalAccessBlacklistRule(ruleId),
    onSuccess: () => {
      setNotice({ kind: "success", message: "规则已删除。" });
      const remaining = Math.max(0, (rules.data?.totalCount ?? 1) - 1);
      const lastPage = Math.max(1, Math.ceil(remaining / PAGE_SIZE));
      if (page > lastPage) setPage(lastPage);
      void queryClient.invalidateQueries({ queryKey: globalRulesQueryKey });
    },
    onError: () => setNotice({ kind: "error", message: "规则删除失败，请重试。" }),
  });

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const validation = validateRuleValue(ruleType, matchValue);
    if (validation) {
      setValidationError(validation);
      setNotice(null);
      return;
    }
    if (createEnabled && !(await confirm(ruleEffectConfirmation(ruleType)))) return;

    setValidationError("");
    setNotice(null);
    const value = ruleType === "CLIENT" ? normalizeClientName(matchValue) : ruleType === "IP_CIDR" ? matchValue.trim() : matchValue;
    createRule.mutate({ ruleType, matchValue: value, isEnabled: createEnabled });
  }

  async function toggleRule(rule: AdminUserAccessBlacklistRule) {
    const isEnabled = !rule.isEnabled;
    if (isEnabled && !(await confirm(ruleEffectConfirmation(rule.ruleType)))) return;
    setNotice(null);
    setRuleEnabled.mutate({ ruleId: rule.id, isEnabled });
  }

  async function confirmDelete(rule: AdminUserAccessBlacklistRule) {
    if (!(await confirm(`确认删除这条${ruleTypeLabel(rule.ruleType)}规则？删除后该匹配值不再阻止登录、已有会话或播放授权。`))) return;
    setNotice(null);
    deleteRule.mutate(rule.id);
  }

  const totalPages = Math.max(1, Math.ceil((rules.data?.totalCount ?? 0) / PAGE_SIZE));
  const mutationPending = createRule.isPending || setRuleEnabled.isPending || deleteRule.isPending;

  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel lux-admin-global-access-rules" aria-labelledby="lux-admin-global-access-rules-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-global-access-rules-title">全站黑名单</h2></div>
      </div>
      <p className="lux-admin-muted">规则命中时会阻止登录、撤销或拒绝已有会话，并阻止播放授权；全站规则适用于所有用户。</p>
      <p className="lux-admin-muted">IP/CIDR 只依据连接 peer 或已配置的可信代理链判定，不直接信任客户端提交的转发头。保存的地址和网段由服务器校验并规范化。</p>

      <form className="lux-admin-form lux-admin-user-profile-form lux-admin-global-access-form" onSubmit={submit} aria-busy={createRule.isPending}>
        <label>规则类型
          <LuxSelectField aria-label="规则类型" value={ruleType} disabled={createRule.isPending} onChange={(event) => { setRuleType(event.target.value as AdminUserAccessBlacklistRuleType); setValidationError(""); setNotice(null); }}>
            <option value="DEVICE">设备 ID</option>
            <option value="CLIENT">客户端名称</option>
            <option value="IP_CIDR">IP/CIDR</option>
          </LuxSelectField>
        </label>
        <label>匹配值
          <LuxTextField aria-label="匹配值" value={matchValue} maxLength={256} disabled={createRule.isPending} onChange={(event) => { setMatchValue(event.target.value); setValidationError(""); setNotice(null); }} placeholder={ruleType === "DEVICE" ? "输入设备 ID" : ruleType === "CLIENT" ? "输入客户端名称" : "例如 192.0.2.0/24 或 2001:db8::/32"} />
        </label>
        <LuxSwitch className="lux-admin-toggle" checked={createEnabled} disabled={createRule.isPending} onChange={(event) => setCreateEnabled(event.target.checked)} label="创建后立即启用" />
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="primary" type="submit" disabled={createRule.isPending}>{createRule.isPending ? "正在保存…" : "添加全站规则"}</LuxButton>
        </div>
        <p className="lux-admin-muted">设备 ID 精确匹配；客户端名称忽略大小写并折叠连续空白；IP/CIDR 由服务器转换为规范的网络地址。</p>
        {validationError ? <p className="lux-error-copy" role="alert">{validationError}</p> : null}
      </form>

      {notice ? <p className={notice.kind === "error" ? "lux-error-copy" : "lux-account-notice"} role={notice.kind === "error" ? "alert" : "status"}>{notice.message}</p> : null}

      {rules.isPending ? <p className="lux-admin-muted" role="status">正在读取全站规则…</p> : null}
      {rules.isError ? (
        <div className="lux-admin-access-block" role="alert">
          <p>读取全站规则失败，请重试。</p>
          <LuxButton tone="secondary" type="button" onClick={() => void rules.refetch()} disabled={rules.isFetching}>重新加载</LuxButton>
        </div>
      ) : null}
      {rules.data && rules.data.rules.length === 0 ? <p className="lux-admin-muted" role="status">当前没有全站规则。</p> : null}
      {rules.data && rules.data.rules.length > 0 ? (
        <>
          <div className="lux-admin-user-session-table-wrap">
            <table className="lux-admin-user-session-table lux-admin-global-access-rule-table">
              <thead><tr><th scope="col">类型</th><th scope="col">匹配值</th><th scope="col">状态</th><th scope="col">创建时间</th><th scope="col">操作</th></tr></thead>
              <tbody>
                {rules.data.rules.map((rule) => (
                  <tr key={rule.id}>
                    <th scope="row">{ruleTypeLabel(rule.ruleType)}</th>
                    <td><code>{rule.matchValue}</code></td>
                    <td><span className={rule.isEnabled ? "lux-admin-device-login-status is-active" : "lux-admin-device-login-status is-inactive"}>{rule.isEnabled ? "已启用" : "已停用"}</span></td>
                    <td><time dateTime={new Date(rule.createdAt * 1000).toISOString()}>{new Date(rule.createdAt * 1000).toLocaleString()}</time></td>
                    <td>
                      <div className="lux-admin-user-profile-actions">
                        <LuxButton tone="secondary" type="button" aria-label={`${rule.isEnabled ? "停用" : "启用"}规则 ${rule.id}`} onClick={() => toggleRule(rule)} disabled={mutationPending}>{rule.isEnabled ? "停用" : "启用"}</LuxButton>
                        <LuxButton tone="secondary" type="button" aria-label={`删除规则 ${rule.id}`} onClick={() => confirmDelete(rule)} disabled={mutationPending}>删除</LuxButton>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="lux-admin-pagination">
            <span>第 {page} / {totalPages} 页 · {rules.data.totalCount} 条全站规则（每页最多 {PAGE_SIZE} 条）</span>
            <div>
              <LuxButton tone="secondary" type="button" onClick={() => setPage((current) => Math.max(1, current - 1))} disabled={page <= 1}>上一页</LuxButton>
              <LuxButton tone="secondary" type="button" onClick={() => setPage((current) => Math.min(totalPages, current + 1))} disabled={page >= totalPages}>下一页</LuxButton>
            </div>
          </div>
        </>
      ) : null}
    </section>
  );
}

function ruleTypeLabel(ruleType: AdminUserAccessBlacklistRuleType) {
  switch (ruleType) {
    case "DEVICE": return "设备 ID";
    case "CLIENT": return "客户端名称";
    case "IP_CIDR": return "IP/CIDR";
  }
}

function ruleEffectConfirmation(ruleType: AdminUserAccessBlacklistRuleType) {
  const scope = ruleType === "IP_CIDR" ? "IP/CIDR 按连接 peer 或已配置的可信代理链匹配。" : "";
  return `启用后，命中该规则会阻止登录、撤销或拒绝已有会话，并阻止播放授权。${scope}确定继续吗？`;
}

function validateRuleValue(ruleType: AdminUserAccessBlacklistRuleType, value: string): string | null {
  if (new TextEncoder().encode(value).byteLength > 256) return "匹配值不能超过 256 个 UTF-8 字节。";
  if (controlCharacterPattern.test(value)) return "匹配值不能包含控制字符。";
  if (!value.trim()) return "请输入匹配值。";
  if (ruleType === "CLIENT" && new TextEncoder().encode(normalizeClientName(value)).byteLength > 256) {
    return "规范化后的客户端名称超过 256 个 UTF-8 字节。";
  }
  if (ruleType === "IP_CIDR" && !isValidIpCidr(value)) return "IP 地址或前缀长度无效。";
  return null;
}

function normalizeClientName(value: string) {
  return value.split(/\s+/u).filter(Boolean).join(" ").toLowerCase();
}

function isValidIpCidr(value: string) {
  const trimmed = value.trim();
  const parts = trimmed.split("/");
  if (parts.length > 2 || !parts[0]) return false;
  const address = parseIpAddress(parts[0]);
  if (!address) return false;
  if (parts.length === 1) return true;
  if (!/^\d+$/u.test(parts[1])) return false;
  const prefix = Number(parts[1]);
  return Number.isSafeInteger(prefix) && prefix >= 0 && prefix <= address.bits;
}

function parseIpAddress(value: string): { bits: 32 | 128 } | null {
  if (value.includes(":")) return isValidIpv6(value) ? { bits: 128 } : null;
  const octets = value.split(".");
  if (octets.length !== 4 || octets.some((octet) => !/^\d+$/u.test(octet))) return null;
  if (octets.some((octet) => Number(octet) > 255)) return null;
  return { bits: 32 };
}

function isValidIpv6(value: string) {
  if (value.includes("%")) return false;
  let address = value;
  if (address.includes(".")) {
    const lastColon = address.lastIndexOf(":");
    if (lastColon < 0) return false;
    const ipv4 = address.slice(lastColon + 1);
    const octets = ipv4.split(".");
    if (octets.length !== 4 || octets.some((octet) => !/^\d+$/u.test(octet) || Number(octet) > 255)) return false;
    const value32 = octets.reduce((value, octet) => (value << 8n) | BigInt(Number(octet)), 0n);
    const high = Number((value32 >> 16n) & 0xffffn).toString(16);
    const low = Number(value32 & 0xffffn).toString(16);
    address = `${address.slice(0, lastColon)}:${high}:${low}`;
  }

  const compressedAt = address.indexOf("::");
  if (compressedAt !== address.lastIndexOf("::")) return false;
  const leftText = compressedAt < 0 ? address : address.slice(0, compressedAt);
  const rightText = compressedAt < 0 ? "" : address.slice(compressedAt + 2);
  const left = leftText ? leftText.split(":") : [];
  const right = rightText ? rightText.split(":") : [];
  const groups = [...left, ...right];
  if (groups.some((group) => !/^[\da-f]{1,4}$/iu.test(group))) return false;
  if (compressedAt < 0) return groups.length === 8;
  return groups.length < 8;
}
