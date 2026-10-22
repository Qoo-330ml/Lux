import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { FormEvent } from "react";
import { useEffect, useState } from "react";
import { ApiError, api } from "../../lib/api/client";
import type { AdminUserAccessBlacklistRule, AdminUserAccessBlacklistRuleType, AdminUserDevice } from "../../lib/api/types";

const RULE_PAGE_SIZE = 20;
const DEVICE_PAGE_SIZE = 100;
const controlCharacterPattern = /[\u0000-\u001f\u007f-\u009f]/u;

type Notice = { kind: "success" | "error"; message: string };
type CreateInput = { ruleType: AdminUserAccessBlacklistRuleType; matchValue: string; isEnabled: boolean };
type RetryAction =
  | { type: "create"; input: CreateInput }
  | { type: "toggle"; ruleId: string; isEnabled: boolean }
  | { type: "delete"; ruleId: string };

export function AdminUserAccessRulesPanel({ userId }: { userId: string }) {
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [devicePage, setDevicePage] = useState(1);
  const [ruleType, setRuleType] = useState<AdminUserAccessBlacklistRuleType>("DEVICE");
  const [selectedDeviceId, setSelectedDeviceId] = useState("");
  const [matchValue, setMatchValue] = useState("");
  const [createEnabled, setCreateEnabled] = useState(true);
  const [validationError, setValidationError] = useState("");
  const [notice, setNotice] = useState<Notice | null>(null);
  const [retryAction, setRetryAction] = useState<RetryAction | null>(null);
  const ruleQueryKey = ["admin", "user", userId, "access-blacklist"] as const;
  const rules = useQuery({
    queryKey: [...ruleQueryKey, page],
    queryFn: () => api.adminUserAccessBlacklistRules(userId, page, RULE_PAGE_SIZE),
    retry: false,
  });
  const devices = useQuery({
    queryKey: ["admin", "user", userId, "access-blacklist-device-options", devicePage],
    queryFn: () => api.adminUserDevices(userId, devicePage, DEVICE_PAGE_SIZE),
    retry: false,
  });

  useEffect(() => {
    if (devices.data && !devices.data.devices.some((device) => device.deviceId === selectedDeviceId)) {
      setSelectedDeviceId("");
    }
  }, [devices.data, selectedDeviceId]);

  const createRule = useMutation({
    mutationFn: (input: CreateInput) => api.createAdminUserAccessBlacklistRule(userId, input),
    onSuccess: (created) => {
      setMatchValue("");
      setSelectedDeviceId("");
      setRuleType("DEVICE");
      setCreateEnabled(true);
      setValidationError("");
      setRetryAction(null);
      setNotice({ kind: "success", message: `规则已创建，服务器匹配值：${created.matchValue}` });
      setPage(1);
      void queryClient.invalidateQueries({ queryKey: ruleQueryKey });
    },
    onError: (error, input) => {
      setRetryAction(error instanceof ApiError && error.status === 409 ? null : { type: "create", input });
      setNotice({
        kind: "error",
        message: error instanceof ApiError && error.status === 409
          ? "相同范围和匹配值的规则已存在，请检查列表。"
          : "规则创建失败，请重试或检查输入。",
      });
    },
  });

  const setRuleEnabled = useMutation({
    mutationFn: ({ ruleId, isEnabled }: { ruleId: string; isEnabled: boolean }) =>
      api.setAdminUserAccessBlacklistRuleEnabled(userId, ruleId, isEnabled),
    onSuccess: ({ isEnabled }) => {
      setRetryAction(null);
      setNotice({ kind: "success", message: isEnabled ? "规则已启用。" : "规则已停用。" });
      void queryClient.invalidateQueries({ queryKey: ruleQueryKey });
    },
    onError: (_error, input) => {
      setRetryAction({ type: "toggle", ...input });
      setNotice({ kind: "error", message: "规则状态更新失败，请重试。" });
    },
  });

  const deleteRule = useMutation({
    mutationFn: (ruleId: string) => api.deleteAdminUserAccessBlacklistRule(userId, ruleId),
    onSuccess: () => {
      setRetryAction(null);
      setNotice({ kind: "success", message: "规则已删除。" });
      const remaining = Math.max(0, (rules.data?.totalCount ?? 1) - 1);
      const lastPage = Math.max(1, Math.ceil(remaining / RULE_PAGE_SIZE));
      if (page > lastPage) setPage(lastPage);
      void queryClient.invalidateQueries({ queryKey: ruleQueryKey });
    },
    onError: (_error, ruleId) => {
      setRetryAction({ type: "delete", ruleId });
      setNotice({ kind: "error", message: "规则删除失败，请重试。" });
    },
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = ruleType === "DEVICE" ? selectedDeviceId : matchValue;
    const validation = validateRuleValue(ruleType, value);
    if (validation) {
      setValidationError(validation);
      setNotice(null);
      return;
    }
    if (createEnabled && !window.confirm(ruleEffectConfirmation(ruleType))) return;

    setValidationError("");
    setNotice(null);
    setRetryAction(null);
    const normalizedValue = ruleType === "CLIENT" ? normalizeClientName(value) : ruleType === "IP_CIDR" ? value.trim() : value;
    createRule.mutate({ ruleType, matchValue: normalizedValue, isEnabled: createEnabled });
  }

  function toggleRule(rule: AdminUserAccessBlacklistRule) {
    const isEnabled = !rule.isEnabled;
    if (isEnabled && !window.confirm(ruleEffectConfirmation(rule.ruleType))) return;
    setNotice(null);
    setRetryAction(null);
    setRuleEnabled.mutate({ ruleId: rule.id, isEnabled });
  }

  function confirmDelete(rule: AdminUserAccessBlacklistRule) {
    if (!window.confirm(`确认删除这条${ruleTypeLabel(rule.ruleType)}规则？删除后该账号的匹配值不再被阻止。`)) return;
    setNotice(null);
    setRetryAction(null);
    deleteRule.mutate(rule.id);
  }

  function retryFailedOperation() {
    if (!retryAction) return;
    const action = retryAction;
    setRetryAction(null);
    setNotice(null);
    if (action.type === "create") createRule.mutate(action.input);
    else if (action.type === "toggle") setRuleEnabled.mutate({ ruleId: action.ruleId, isEnabled: action.isEnabled });
    else deleteRule.mutate(action.ruleId);
  }

  const totalPages = Math.max(1, Math.ceil((rules.data?.totalCount ?? 0) / RULE_PAGE_SIZE));
  const deviceTotalPages = Math.max(1, Math.ceil((devices.data?.totalCount ?? 0) / DEVICE_PAGE_SIZE));
  const mutationPending = createRule.isPending || setRuleEnabled.isPending || deleteRule.isPending;

  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel lux-admin-user-access-rules" aria-labelledby="lux-admin-user-access-rules-title" aria-busy={mutationPending}>
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-access-rules-title">此用户访问黑名单</h2></div>
      </div>
      <p className="lux-admin-muted">规则仅应用于此账号，不影响其他用户。命中时会拒绝该账号登录、撤销或拒绝其已有会话，并阻止其播放授权。</p>
      <p className="lux-admin-muted">IP/CIDR 只依据连接 peer 或已配置的可信代理链判定，不直接信任客户端提交的转发头。</p>

      <form className="lux-admin-form lux-admin-user-profile-form lux-admin-user-access-form" onSubmit={submit} aria-busy={createRule.isPending}>
        <label>规则类型
          <select aria-label="规则类型" value={ruleType} disabled={mutationPending} onChange={(event) => { setRuleType(event.target.value as AdminUserAccessBlacklistRuleType); setValidationError(""); setNotice(null); setRetryAction(null); }}>
            <option value="DEVICE">设备 ID</option>
            <option value="CLIENT">客户端名称</option>
            <option value="IP_CIDR">IP/CIDR</option>
          </select>
        </label>
        {ruleType === "DEVICE" ? (
          <label>已记录设备
            <select aria-label="已记录设备" value={selectedDeviceId} disabled={mutationPending || devices.isPending || devices.isError || (devices.data?.devices.length ?? 0) === 0} onChange={(event) => { setSelectedDeviceId(event.target.value); setValidationError(""); setNotice(null); setRetryAction(null); }}>
              <option value="">选择已记录设备</option>
              {(devices.data?.devices ?? []).map((device) => <option key={device.deviceId} value={device.deviceId}>{deviceOptionLabel(device)}</option>)}
            </select>
          </label>
        ) : (
          <label>匹配值
            <input aria-label="匹配值" value={matchValue} maxLength={256} disabled={mutationPending} onChange={(event) => { setMatchValue(event.target.value); setValidationError(""); setNotice(null); setRetryAction(null); }} placeholder={ruleType === "CLIENT" ? "输入客户端名称" : "例如 192.0.2.0/24 或 2001:db8::/32"} />
          </label>
        )}
        <label className="lux-admin-toggle"><input type="checkbox" checked={createEnabled} disabled={mutationPending} onChange={(event) => setCreateEnabled(event.target.checked)} />创建后立即启用</label>
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-primary" type="submit" disabled={mutationPending}>{createRule.isPending ? "正在保存…" : "添加此用户规则"}</button>
        </div>
        {devices.isPending ? <p className="lux-admin-muted" role="status">正在读取此用户的设备选项…</p> : null}
        {devices.isError ? (
          <div className="lux-admin-access-block" role="alert">
            <p>读取设备列表失败。设备规则暂不可选，其他类型规则仍可管理。</p>
            <button className="lux-button lux-button-secondary" type="button" onClick={() => void devices.refetch()} disabled={devices.isFetching}>重试读取设备</button>
          </div>
        ) : null}
        {devices.data && devices.data.totalCount === 0 ? <p className="lux-admin-muted" role="status">此账号尚无已记录设备。</p> : null}
        {devices.data && devices.data.totalCount > DEVICE_PAGE_SIZE ? (
          <div className="lux-admin-pagination" aria-label="设备选项分页">
            <span>设备选项第 {devices.data.page} / {deviceTotalPages} 页</span>
            <div>
              <button className="lux-button lux-button-secondary" type="button" onClick={() => setDevicePage((current) => Math.max(1, current - 1))} disabled={devices.isFetching || devicePage <= 1}>上一页</button>
              <button className="lux-button lux-button-secondary" type="button" onClick={() => setDevicePage((current) => Math.min(deviceTotalPages, current + 1))} disabled={devices.isFetching || devicePage >= deviceTotalPages}>下一页</button>
            </div>
          </div>
        ) : null}
        <p className="lux-admin-muted">设备规则只能选择此账号已记录的设备；客户端名称忽略大小写并折叠连续空白；IP/CIDR 由服务器校验并规范化。</p>
        {validationError ? <p className="lux-error-copy" role="alert">{validationError}</p> : null}
      </form>

      {notice ? <p className={notice.kind === "error" ? "lux-error-copy" : "lux-account-notice"} role={notice.kind === "error" ? "alert" : "status"}>{notice.message}</p> : null}
      {notice?.kind === "error" && retryAction ? <button className="lux-button lux-button-secondary" type="button" onClick={retryFailedOperation} disabled={mutationPending}>重试上次失败操作</button> : null}

      {rules.isPending ? <p className="lux-admin-muted" role="status">正在读取此用户的访问规则…</p> : null}
      {rules.isError ? (
        <div className="lux-admin-access-block" role="alert">
          <p>读取此用户的访问规则失败，请重试。</p>
          <button className="lux-button lux-button-secondary" type="button" onClick={() => void rules.refetch()} disabled={rules.isFetching}>重新加载访问规则</button>
        </div>
      ) : null}
      {rules.data && rules.data.rules.length === 0 ? <p className="lux-admin-muted" role="status">当前没有此用户的访问规则。</p> : null}
      {rules.data && rules.data.rules.length > 0 ? (
        <>
          <div className="lux-admin-user-session-table-wrap">
            <table className="lux-admin-user-session-table lux-admin-user-access-rule-table">
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
                        <button className="lux-button lux-button-secondary" type="button" aria-label={`${rule.isEnabled ? "停用" : "启用"}规则 ${rule.id}`} onClick={() => toggleRule(rule)} disabled={mutationPending}>{rule.isEnabled ? "停用" : "启用"}</button>
                        <button className="lux-button lux-button-secondary" type="button" aria-label={`删除规则 ${rule.id}`} onClick={() => confirmDelete(rule)} disabled={mutationPending}>删除</button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="lux-admin-pagination">
            <span>第 {page} / {totalPages} 页 · {rules.data.totalCount} 条此用户规则（每页最多 {RULE_PAGE_SIZE} 条）</span>
            <div>
              <button className="lux-button lux-button-secondary" type="button" onClick={() => setPage((current) => Math.max(1, current - 1))} disabled={page <= 1}>上一页</button>
              <button className="lux-button lux-button-secondary" type="button" onClick={() => setPage((current) => Math.min(totalPages, current + 1))} disabled={page >= totalPages}>下一页</button>
            </div>
          </div>
        </>
      ) : null}
    </section>
  );
}

function deviceOptionLabel(device: AdminUserDevice) {
  return `${device.clientName} · ${device.deviceName || "未命名设备"} · ${device.deviceId}`;
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
  return `启用后，命中该规则会拒绝此账号登录、撤销或拒绝其已有会话，并阻止其播放授权。其他用户不受影响。${scope}确定继续吗？`;
}

function validateRuleValue(ruleType: AdminUserAccessBlacklistRuleType, value: string): string | null {
  if (new TextEncoder().encode(value).byteLength > 256) return "匹配值不能超过 256 个 UTF-8 字节。";
  if (controlCharacterPattern.test(value)) return "匹配值不能包含控制字符。";
  if (!value.trim()) return ruleType === "DEVICE" ? "请选择此账号已记录的设备。" : "请输入匹配值。";
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
    const octets = address.slice(lastColon + 1).split(".");
    if (octets.length !== 4 || octets.some((octet) => !/^\d+$/u.test(octet) || Number(octet) > 255)) return false;
    const ipv4Value = octets.reduce((value, octet) => (value << 8n) | BigInt(Number(octet)), 0n);
    const high = Number((ipv4Value >> 16n) & 0xffffn).toString(16);
    const low = Number(ipv4Value & 0xffffn).toString(16);
    address = `${address.slice(0, lastColon)}:${high}:${low}`;
  }

  const compressedAt = address.indexOf("::");
  if (compressedAt !== address.lastIndexOf("::")) return false;
  const leftText = compressedAt < 0 ? address : address.slice(0, compressedAt);
  const rightText = compressedAt < 0 ? "" : address.slice(compressedAt + 2);
  const groups = [...(leftText ? leftText.split(":") : []), ...(rightText ? rightText.split(":") : [])];
  if (groups.some((group) => !/^[\da-f]{1,4}$/iu.test(group))) return false;
  return compressedAt < 0 ? groups.length === 8 : groups.length < 8;
}
