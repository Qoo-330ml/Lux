import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useState } from "react";
import { api } from "../../../../lib/api/client";
import type { AdminUser, AdminUserPlaybackPolicy } from "../../../../lib/api/types";
import { LuxButton, LuxSwitch, LuxTextField } from "../../../../components/ui";
import { AdminUserManagementState } from "../AdminUserManagementState";

export function AdminUserPlaybackPolicySection({ user }: { user: AdminUser }) {
  if (user.deletedAt != null) {
    return (
      <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-playback-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-playback-title">播放与转码</h2></div></div>
        <p className="lux-admin-muted">回收站中的账号播放策略为只读，恢复后可查看。</p>
      </section>
    );
  }
  return <AdminUserPlaybackPolicyEditor user={user} />;
}

function AdminUserPlaybackPolicyEditor({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const queryKey = ["admin", "user", user.id, "playback-policy"];
  const policyQuery = useQuery({
    queryKey,
    queryFn: () => api.adminUserPlaybackPolicy(user.id),
  });
  const [draft, setDraft] = useState<AdminUserPlaybackPolicy | null>(null);
  const [limitDraft, setLimitDraft] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const save = useMutation({
    mutationFn: (policy: AdminUserPlaybackPolicy) => api.updateAdminUserPlaybackPolicy(user.id, policy),
    onSuccess: ({ policy }) => {
      setDraft(policy);
      setLimitDraft(String(policy.maxSimultaneousPlaybacks));
      setError("");
      setNotice("播放策略已保存。");
      queryClient.setQueryData(queryKey, { policy });
    },
    onError: (cause) => {
      setNotice("");
      setError(cause.message || "播放策略保存失败，请重试。");
    },
  });
  const currentPolicy = policyQuery.data?.policy;
  const currentDraft = draft ?? currentPolicy;
  const limitInput = limitDraft ?? (currentPolicy ? String(currentPolicy.maxSimultaneousPlaybacks) : "");
  const parsedLimit = Number(limitInput);
  const validLimit = limitInput.trim() !== "" && Number.isSafeInteger(parsedLimit) && parsedLimit >= 0;
  const updatedDraft = currentDraft && validLimit ? { ...currentDraft, maxSimultaneousPlaybacks: parsedLimit } : currentDraft;
  const hasChanges = updatedDraft != null && currentPolicy != null && (
    updatedDraft.canPlay !== currentPolicy.canPlay
    || updatedDraft.maxSimultaneousPlaybacks !== currentPolicy.maxSimultaneousPlaybacks
    || updatedDraft.allowRemux !== currentPolicy.allowRemux
    || updatedDraft.allowAudioTranscode !== currentPolicy.allowAudioTranscode
    || updatedDraft.allowVideoTranscode !== currentPolicy.allowVideoTranscode
  );

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!updatedDraft || !validLimit) return;
    setNotice("");
    setError("");
    save.mutate(updatedDraft);
  }

  if (policyQuery.isPending) return <AdminUserManagementState label="正在读取播放策略…" />;
  if (policyQuery.error) return <AdminUserManagementState label={policyQuery.error.message || "暂时无法读取播放策略。"} error />;
  if (!currentDraft) return <AdminUserManagementState label="暂时无法读取播放策略。" error />;

  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-playback-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-playback-title">播放与转码</h2></div></div>
      <form className="lux-admin-form lux-admin-user-playback-form" onSubmit={submit}>
        <fieldset className="lux-admin-user-playback-options">
          <legend className="lux-admin-muted">播放权限</legend>
          <LuxSwitch className="lux-admin-user-playback-toggle" aria-label="允许播放" checked={currentDraft.canPlay} onChange={(event) => setDraft({ ...currentDraft, canPlay: event.currentTarget.checked })} label="允许播放" />
          <LuxSwitch className="lux-admin-user-playback-toggle" aria-label="允许封装转换" checked={currentDraft.allowRemux} onChange={(event) => setDraft({ ...currentDraft, allowRemux: event.currentTarget.checked })} label="允许封装转换" />
          <LuxSwitch className="lux-admin-user-playback-toggle" aria-label="允许音频转码" checked={currentDraft.allowAudioTranscode} onChange={(event) => setDraft({ ...currentDraft, allowAudioTranscode: event.currentTarget.checked })} label="允许音频转码" />
          <LuxSwitch className="lux-admin-user-playback-toggle" aria-label="允许视频转码" checked={currentDraft.allowVideoTranscode} onChange={(event) => setDraft({ ...currentDraft, allowVideoTranscode: event.currentTarget.checked })} label="允许视频转码" />
        </fieldset>
        <label className="lux-admin-user-playback-limit">
          同时播放上限
          <LuxTextField
            type="number"
            aria-label="同时播放上限"
            aria-invalid={!validLimit}
            aria-describedby="lux-admin-user-playback-limit-help"
            min={0}
            step={1}
            value={limitInput}
            onChange={(event) => setLimitDraft(event.currentTarget.value)}
          />
        </label>
        <p id="lux-admin-user-playback-limit-help" className="lux-admin-muted">0 表示不限。并发上限只控制此账号的同时播放数。</p>
        <p className="lux-admin-muted">新建播放会话会按当前策略校验准入、并发和转码；已签发的服务端 HLS 资源会在每次请求时重新检查，STRM 只允许直放。</p>
        {limitInput.trim() === "" || !validLimit ? <p className="lux-admin-user-playback-validation" role="alert">请输入非负整数。</p> : null}
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="primary" type="submit" disabled={!hasChanges || !validLimit || save.isPending}>
            {save.isPending ? "保存中…" : "保存播放策略"}
          </LuxButton>
          {error ? <span role="alert">{error}</span> : null}
          {notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}
