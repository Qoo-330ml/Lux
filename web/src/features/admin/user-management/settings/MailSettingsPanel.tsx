import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useEffect, useState } from "react";
import { api } from "../../../../lib/api/client";
import type { AdminMailSettingsResponse, MailSettings, SmtpTlsMode } from "../../../../lib/api/types";
import { LuxButton, LuxSelectField, LuxTextField, useLuxConfirm } from "../../../../components/ui";

const adminMailSettingsQueryKey = ["admin", "settings", "mail"] as const;

type MailSettingsDraft = Omit<MailSettings, "port"> & { port: string };

function emptyMailSettingsDraft(): MailSettingsDraft {
  return { host: "", port: "465", tlsMode: "SMTPS", senderName: "Lux", senderEmail: "", siteUrl: "" };
}

function mailSettingsDraft(settings: MailSettings | null): MailSettingsDraft {
  return settings ? { ...settings, port: String(settings.port) } : emptyMailSettingsDraft();
}

function mailSettingsInput(draft: MailSettingsDraft): MailSettings {
  return { ...draft, port: Number(draft.port) };
}

function isValidMailSettingsDraft(draft: MailSettingsDraft) {
  const port = Number(draft.port);
  let siteUrl: URL;
  try {
    siteUrl = new URL(draft.siteUrl);
  } catch {
    return false;
  }
  return draft.host.trim().length > 0
    && draft.host.length <= 253
    && Number.isInteger(port)
    && port >= 1
    && port <= 65_535
    && draft.senderName.trim().length > 0
    && draft.senderEmail.includes("@")
    && (siteUrl.protocol === "http:" || siteUrl.protocol === "https:")
    && !siteUrl.username
    && !siteUrl.password
    && !siteUrl.search
    && !siteUrl.hash;
}

export function AdminUserMailSettings() {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const settingsQuery = useQuery({
    queryKey: adminMailSettingsQueryKey,
    queryFn: () => api.adminMailSettings(),
  });
  const [draft, setDraft] = useState<MailSettingsDraft>(emptyMailSettingsDraft);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [savingCredentials, setSavingCredentials] = useState(false);
  const [settingsNotice, setSettingsNotice] = useState("");
  const [credentialsNotice, setCredentialsNotice] = useState("");
  const [credentialsError, setCredentialsError] = useState(false);
  const [connectionNotice, setConnectionNotice] = useState("");
  const mailSettings = settingsQuery.data?.settings ?? null;
  const settingsInput = mailSettingsInput(draft);
  const settingsChanged = JSON.stringify(settingsInput) !== JSON.stringify(mailSettings);
  const validSettings = isValidMailSettingsDraft(draft);
  const hasCredentialInput = Boolean(username || password);
  const validCredentialPair = username.trim().length > 0 && password.length > 0;

  useEffect(() => {
    setDraft(mailSettingsDraft(mailSettings));
  }, [settingsQuery.data?.settings]);

  const saveSettings = useMutation({
    mutationFn: (input: MailSettings) => api.updateAdminMailSettings(input),
    onSuccess: (_result, input) => {
      queryClient.setQueryData<AdminMailSettingsResponse>(adminMailSettingsQueryKey, (current) => ({
        configured: true,
        credentialsConfigured: current?.credentialsConfigured ?? false,
        settings: input,
      }));
      setDraft(mailSettingsDraft(input));
      setSettingsNotice("SMTP 设置已保存。");
    },
    onError: (error) => setSettingsNotice(error.message || "SMTP 设置保存失败，请重试。"),
  });
  const deleteCredentials = useMutation({
    mutationFn: () => api.deleteAdminMailCredentials(),
    onSuccess: () => {
      queryClient.setQueryData<AdminMailSettingsResponse>(adminMailSettingsQueryKey, (current) => current ? ({ ...current, credentialsConfigured: false }) : current);
      setCredentialsNotice("SMTP 凭据已删除。");
    },
    onError: (error) => setCredentialsNotice(error.message || "SMTP 凭据删除失败，请重试。"),
  });
  const testConnection = useMutation({
    mutationFn: () => api.testAdminMailConnection(),
    onSuccess: () => setConnectionNotice("SMTP 连接成功。"),
    onError: (error) => setConnectionNotice(error.message || "SMTP 连接失败，请检查设置后重试。"),
  });

  if (settingsQuery.isPending) return <p className="lux-admin-muted" role="status">正在读取 SMTP 设置…</p>;
  if (settingsQuery.error || !settingsQuery.data) {
    return <div role="alert" className="lux-error-copy">读取 SMTP 设置失败：{settingsQuery.error?.message || "没有收到设置数据"}<LuxButton tone="secondary" type="button" onClick={() => void settingsQuery.refetch()}>重试</LuxButton></div>;
  }

  function saveMailSettings(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!validSettings || !settingsChanged || saveSettings.isPending) return;
    setSettingsNotice("");
    saveSettings.mutate(settingsInput);
  }

  async function submitCredentials(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!validCredentialPair || savingCredentials) return;
    setCredentialsNotice("");
    setCredentialsError(false);
    deleteCredentials.reset();
    setSavingCredentials(true);
    try {
      await api.updateAdminMailCredentials({ username: username.trim(), password });
      queryClient.setQueryData<AdminMailSettingsResponse>(adminMailSettingsQueryKey, (current) => current ? ({ ...current, credentialsConfigured: true }) : current);
      setUsername("");
      setCredentialsNotice("SMTP 凭据已保存；出于安全原因，密码不会回显。");
    } catch (error) {
      setCredentialsError(true);
      setCredentialsNotice(error instanceof Error ? error.message : "SMTP 凭据保存失败，请重试。");
    } finally {
      setPassword("");
      setSavingCredentials(false);
    }
  }

  async function removeCredentials() {
    if (!(await confirm("删除已保存的 SMTP 用户名和密码？"))) return;
    setCredentialsNotice("");
    setCredentialsError(false);
    deleteCredentials.reset();
    deleteCredentials.mutate();
  }

  const saving = saveSettings.isPending;
  const credentialBusy = savingCredentials || deleteCredentials.isPending;
  const connectionBusy = testConnection.isPending;
  const credentialsConfigured = settingsQuery.data.credentialsConfigured;

  return (
    <div className="lux-admin-user-management-page">
      <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-mail-settings-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-mail-settings-title">SMTP 服务</h2></div></div>
        <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={saveMailSettings}>
          <label>SMTP 主机<LuxTextField aria-label="SMTP 主机" value={draft.host} onChange={(event) => { setDraft({ ...draft, host: event.target.value }); setSettingsNotice(""); }} autoComplete="off" maxLength={253} required disabled={saving} /></label>
          <label>SMTP 端口<LuxTextField aria-label="SMTP 端口" type="number" min={1} max={65_535} step={1} value={draft.port} onChange={(event) => { setDraft({ ...draft, port: event.target.value }); setSettingsNotice(""); }} required disabled={saving} /></label>
          <label>传输加密<LuxSelectField aria-label="SMTP TLS 模式" value={draft.tlsMode} onChange={(event) => { setDraft({ ...draft, tlsMode: event.target.value as SmtpTlsMode }); setSettingsNotice(""); }} disabled={saving}><option value="SMTPS">隐式 TLS（SMTPS）</option><option value="STARTTLS">强制 STARTTLS</option></LuxSelectField></label>
          <label>发件人名称<LuxTextField aria-label="SMTP 发件人名称" value={draft.senderName} onChange={(event) => { setDraft({ ...draft, senderName: event.target.value }); setSettingsNotice(""); }} maxLength={128} required disabled={saving} /></label>
          <label>发件人邮箱<LuxTextField aria-label="SMTP 发件人邮箱" type="email" value={draft.senderEmail} onChange={(event) => { setDraft({ ...draft, senderEmail: event.target.value }); setSettingsNotice(""); }} maxLength={320} autoComplete="email" required disabled={saving} /></label>
          <label>站点访问地址<LuxTextField aria-label="SMTP 站点访问地址" type="url" value={draft.siteUrl} onChange={(event) => { setDraft({ ...draft, siteUrl: event.target.value }); setSettingsNotice(""); }} placeholder="https://lux.example.com" required disabled={saving} /></label>
          <p className="lux-admin-muted">仅支持 SMTPS 或强制 STARTTLS。此地址用于后续邮件中的验证链接。</p>
          <div className="lux-admin-user-profile-actions">
            <LuxButton tone="primary" type="submit" disabled={!validSettings || !settingsChanged || saving}>{saving ? "保存中…" : "保存 SMTP 设置"}</LuxButton>
            {settingsNotice ? <span role={saveSettings.isError ? "alert" : "status"}>{saveSettings.error?.message || settingsNotice}</span> : null}
          </div>
        </form>
      </section>

      <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-mail-credentials-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-mail-credentials-title">SMTP 身份验证</h2></div></div>
        <p className="lux-admin-muted" role="status">{credentialsConfigured ? "SMTP 凭据已配置；当前用户名和密码不会显示。" : "SMTP 凭据未配置；如邮件服务器需要认证，请输入用户名和密码。"}</p>
        <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submitCredentials}>
          <label>SMTP 用户名<LuxTextField aria-label="SMTP 用户名" value={username} onChange={(event) => { setUsername(event.target.value); setCredentialsNotice(""); setCredentialsError(false); }} autoComplete="username" maxLength={320} disabled={credentialBusy} /></label>
          <label>SMTP 密码<LuxTextField aria-label="SMTP 密码" type="password" value={password} onChange={(event) => { setPassword(event.target.value); setCredentialsNotice(""); setCredentialsError(false); }} autoComplete="new-password" maxLength={4_096} disabled={credentialBusy} /></label>
          {hasCredentialInput && !validCredentialPair ? <p className="lux-admin-playback-validation" role="alert">请同时填写 SMTP 用户名和密码。</p> : null}
          <div className="lux-admin-user-profile-actions">
            <LuxButton tone="primary" type="submit" disabled={!validCredentialPair || credentialBusy}>{savingCredentials ? "保存中…" : "保存 SMTP 凭据"}</LuxButton>
            {credentialsConfigured ? <LuxButton tone="secondary" type="button" onClick={removeCredentials} disabled={credentialBusy}>{deleteCredentials.isPending ? "删除中…" : "删除 SMTP 凭据"}</LuxButton> : null}
            {credentialsNotice ? <span role={credentialsError || deleteCredentials.isError ? "alert" : "status"}>{deleteCredentials.error?.message || credentialsNotice}</span> : null}
          </div>
        </form>
      </section>

      <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-mail-connection-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-mail-connection-title">连接检查</h2></div></div>
        <p className="lux-admin-muted">连接检查使用最近一次保存的配置，通过 TLS 执行 SMTP NOOP，不会发送测试邮件；单次检查最多等待 8 秒。</p>
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="secondary" type="button" onClick={() => { setConnectionNotice(""); testConnection.mutate(); }} disabled={!settingsQuery.data.configured || settingsChanged || connectionBusy}>{connectionBusy ? "检查中…" : "检查 SMTP 连接"}</LuxButton>
          {connectionNotice ? <span role={testConnection.isError ? "alert" : "status"}>{connectionNotice}</span> : null}
        </div>
      </section>
    </div>
  );
}
