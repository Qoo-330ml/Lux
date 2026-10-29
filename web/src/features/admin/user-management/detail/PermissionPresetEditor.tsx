import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useState } from "react";
import { api } from "../../../../lib/api/client";
import { queryKeys } from "../../../../lib/api/query-keys";
import type { AdminUser } from "../../../../lib/api/types";
import { LuxButton, LuxSelectField, useLuxConfirm } from "../../../../components/ui";

export function AdminUserPermissionPresetEditor({ user }: { user: AdminUser }) {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const presetsQuery = useQuery({
    queryKey: ["admin", "user-permission-presets"],
    queryFn: () => api.adminUserPermissionPresets(),
  });
  const [selectedPresetId, setSelectedPresetId] = useState("");
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const presets = presetsQuery.data?.presets ?? [];
  const selectedPreset = presets.find((preset) => preset.id === selectedPresetId);
  const apply = useMutation({
    mutationFn: (presetId: string) => api.applyAdminUserPermissionPreset(user.id, presetId),
    onSuccess: () => {
      setSelectedPresetId("");
      setError("");
      setNotice("角色预设已应用，用户资料正在刷新。");
      void queryClient.invalidateQueries({ queryKey: ["admin", "user", user.id] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "user-access", user.id] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "user", user.id, "playback-policy"] });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
    },
    onError: (cause) => {
      setNotice("");
      setError(cause.message || "应用角色预设失败，请重试。");
    },
  });
  const presetSource = user.presetSourceDetails;
  const appliedAt = presetSource ? new Date(presetSource.appliedAt * 1000) : null;
  const appliedAtIsValid = appliedAt != null && Number.isFinite(appliedAt.getTime());

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!selectedPreset || apply.isPending) return;
    const displayName = user.displayName || user.usernameNormalized;
    const accepted = await confirm(
      `将“${selectedPreset.name}”应用到“${displayName}”会替换该用户当前的角色、媒体库、下载、播放和转码权限。确定继续吗？`,
    );
    if (!accepted) return;
    setNotice("");
    setError("");
    apply.mutate(selectedPreset.id);
  }

  return (
    <section className="lux-admin-panel lux-admin-user-permission-presets" aria-labelledby="lux-admin-user-permission-presets-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-permission-presets-title">角色预设</h2></div></div>
      <p className="lux-admin-muted">
        当前来源：{" "}
        {presetSource ? (
          <>
            <strong>{presetSource.name}</strong> · 版本 {presetSource.version}
            {appliedAtIsValid && appliedAt ? (
              <> · <time dateTime={appliedAt.toISOString()}>{new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(appliedAt)}</time></>
            ) : null}
          </>
        ) : user.presetSource ? user.presetSource : "尚未应用角色预设。"}
      </p>
      {presetsQuery.isPending ? <p className="lux-admin-muted">正在读取角色预设…</p> : null}
      {presetsQuery.error ? <p role="alert">读取角色预设失败：{presetsQuery.error.message}</p> : null}
      {!presetsQuery.isPending && !presetsQuery.error && presets.length === 0 ? <p className="lux-admin-muted">暂无可用角色预设。</p> : null}
      <form className="lux-admin-form lux-admin-user-preset-form" onSubmit={submit}>
        <label htmlFor={`lux-admin-user-preset-${user.id}`}>
          选择角色预设
          <LuxSelectField
            id={`lux-admin-user-preset-${user.id}`}
            aria-label="选择角色预设"
            value={selectedPresetId}
            disabled={presetsQuery.isPending || Boolean(presetsQuery.error) || presets.length === 0 || apply.isPending}
            onChange={(event) => {
              setSelectedPresetId(event.currentTarget.value);
              setNotice("");
              setError("");
            }}
          >
            <option value="">请选择预设</option>
            {presets.map((preset) => (
              <option key={preset.id} value={preset.id}>{preset.name}{preset.isBuiltin ? "（内置）" : "（自定义）"}</option>
            ))}
          </LuxSelectField>
        </label>
        <p className="lux-admin-muted">应用会覆盖该用户当前的角色、媒体库、下载、播放和转码权限；观看记录、偏好及登录凭证不会改变。</p>
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="primary" type="submit" disabled={!selectedPreset || apply.isPending || Boolean(presetsQuery.error)}>
            {apply.isPending ? "应用中…" : "应用所选预设"}
          </LuxButton>
          {error ? <span role="alert">应用失败：{error}</span> : null}
          {notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}
