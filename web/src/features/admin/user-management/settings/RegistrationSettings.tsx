import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useRef, useState } from "react";
import { ApiError, api } from "../../../../lib/api/client";
import type { AdminRegistrationSettings, RegistrationMode } from "../../../../lib/api/types";
import { LuxButton, LuxSelectField } from "../../../../components/ui";

const adminRegistrationSettingsQueryKey = ["admin", "settings", "registration"] as const;

export function AdminUserRegistrationSettings() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: adminRegistrationSettingsQueryKey, queryFn: () => api.adminRegistrationSettings(), retry: false });
  const [draft, setDraft] = useState<RegistrationMode | null>(null);
  const [notice, setNotice] = useState("");
  const inFlight = useRef(false);
  const update = useMutation({
    mutationFn: (input: AdminRegistrationSettings) => api.updateAdminRegistrationSettings(input),
    onSuccess: (saved) => {
      queryClient.setQueryData(adminRegistrationSettingsQueryKey, saved);
      setDraft(null);
      setNotice("注册模式已保存。");
    },
    onError: async (error) => {
      if (error instanceof ApiError && error.status === 409) {
        const refreshed = await settings.refetch();
        setNotice(refreshed.isError ? "注册模式已被修改，重新读取失败，请重新加载后再保存。" : "已重新读取最新设置，保留了你的选择，请核对后再次保存。");
      } else {
        setNotice("注册模式保存失败，请稍后重试。");
      }
    },
    onSettled: () => { inFlight.current = false; },
  });

  if (settings.isPending) return <p className="lux-admin-muted" role="status">正在读取注册设置…</p>;
  if (!settings.data || settings.isError) return (
    <section className="lux-admin-panel">
      <p className="lux-error-copy" role="alert">读取注册设置失败，请重新加载。</p>
      <LuxButton tone="secondary" type="button" onClick={() => void settings.refetch()} disabled={settings.isFetching}>重新加载注册设置</LuxButton>
    </section>
  );
  const mode = draft ?? settings.data.registrationMode;
  const changed = mode !== settings.data.registrationMode;

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!settings.data || !changed || inFlight.current) return;
    inFlight.current = true;
    setNotice("");
    update.mutate({ registrationMode: mode, version: settings.data.version });
  }

  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-registration-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-registration-title">注册与邀请</h2></div></div>
      <p className="lux-admin-muted">默认采用邀请制，管理员也可以直接创建账户。关闭公开注册不会撤销仍有效的已发邀请。</p>
      <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit} aria-busy={update.isPending} aria-describedby="lux-admin-registration-help">
        <label>注册模式<LuxSelectField aria-label="注册模式" value={mode} disabled={update.isPending} onChange={(event) => { setDraft(event.target.value as RegistrationMode); setNotice(""); update.reset(); }}>
          <option value="INVITATION_ONLY">仅邀请注册</option><option value="PUBLIC_EMAIL">公开邮箱注册</option>
        </LuxSelectField></label>
        <p id="lux-admin-registration-help" className="lux-admin-muted">公开申请需先验证邮箱，再等待管理员审批。账户权限由管理员分配，申请者不能自行选择权限。</p>
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="primary" type="submit" disabled={!changed || update.isPending}>{update.isPending ? "保存中…" : "保存注册模式"}</LuxButton>
          {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}
