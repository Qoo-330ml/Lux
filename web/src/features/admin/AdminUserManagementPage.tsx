import { ArrowLeft } from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useEffect, useRef, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { ApiError, api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminMailSettingsResponse, AdminRegistrationSettings, RegistrationMode, AdminSettings, AdminUserLibraryAccessMode, AdminUserPermissionPreset, AdminUserPermissionPresetInput, MailSettings, NewUserPermissions, SmtpTlsMode } from "../../lib/api/types";
import { UsersWorkspace, type PolicySectionKey } from "./users/UsersWorkspace";
import { formatRelativeTime, formatTimestamp } from "./users/format";
import { AdminUserGlobalAccessRulesPanel } from "./AdminUserGlobalAccessRulesPanel";
import { AdminUserManagementState } from "./user-management/AdminUserManagementState";
import { AdminUserSessionsSection } from "./user-management/detail/sessions/SessionsSection";
import { AdminUserActivityPanel } from "./user-management/detail/ActivityPanel";
import { AdminUserLifecycleActions } from "./user-management/detail/LifecycleActions";
import { AdminUserPermissionsSection } from "./user-management/detail/PermissionsSection";
import { AdminUserPlaybackPolicySection } from "./user-management/detail/PlaybackPolicySection";
import { AdminUserProfileEditor } from "./user-management/detail/ProfileEditor";
import { AdminUserContentRestrictionSection } from "./user-management/detail/ContentRestrictionSection";
import { AdminUserPasswordPanel } from "./user-management/detail/PasswordPanel";
import { AdminUserPreferencesSection } from "./user-management/detail/PreferencesPanel";
import "./user-management.css";
import { LuxButton, LuxCheckbox, LuxRadio, LuxSelectField, LuxTextField, useLuxConfirm } from "../../components/ui";

const detailSections = [
  { id: "overview", label: "概览" },
  { id: "access", label: "访问权限" },
  { id: "devices", label: "设备与访问" },
  { id: "activity", label: "活动记录" },
  { id: "preferences", label: "偏好" },
] as const;

type DetailSectionId = (typeof detailSections)[number]["id"];

const detailSectionAliases: Record<string, DetailSectionId> = {
  profile: "overview",
  permissions: "access",
  content: "access",
  playback: "access",
  overview: "overview",
  access: "access",
  devices: "devices",
  activity: "activity",
  preferences: "preferences",
};

export function AdminUserDetailPage() {
  const { userId = "" } = useParams();
  const [searchParams, setSearchParams] = useSearchParams();
  const requestedSection = searchParams.get("section") ?? "overview";
  const sectionId: DetailSectionId = detailSectionAliases[requestedSection] ?? "overview";
  const userQuery = useQuery({
    queryKey: ["admin", "user", userId],
    queryFn: () => api.adminUser(userId),
    enabled: Boolean(userId),
  });
  const user = userQuery.data?.user;

  if (userQuery.isPending) return <AdminUserManagementState label="正在读取账户资料…" />;
  if (userQuery.error) return <AdminUserManagementState label={userQuery.error.message || "暂时无法读取账户资料。"} error />;
  if (!user) return <AdminUserManagementState label="找不到此账户，或账户已不存在。" error />;

  const displayName = user.displayName || user.usernameNormalized;
  const statusLabel = user.deletedAt != null ? "回收站" : user.isDisabled ? "已禁用" : "正常";
  const facts = [
    `@${user.usernameNormalized}`,
    user.email ? `${user.email}${user.emailVerifiedAt ? "（已验证）" : "（未验证）"}` : "未填写邮箱",
    `最近活动 ${formatRelativeTime(user.lastActivityAt)}`,
    user.createdAt ? `加入于 ${formatTimestamp(user.createdAt)}` : null,
  ].filter((fact): fact is string => Boolean(fact));

  return (
    <div className="lux-admin-page lux-users-page lux-users-detail">
      <Link className="lux-users-detail-back" to="/admin/users"><ArrowLeft size={15} aria-hidden="true" />用户与权限</Link>
      <header className="lux-users-detail-head">
        <div className="lux-users-detail-identity">
          <span className="lux-users-avatar is-large" aria-hidden="true">{avatarInitial(displayName)}</span>
          <div className="lux-users-detail-copy">
            <div className="lux-users-detail-name">
              <h1>{displayName}</h1>
              <span className={user.deletedAt != null || user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>{statusLabel}</span>
              {user.canManageServer ? <span className="lux-users-tag is-strong">管理员</span> : null}
            </div>
            <p className="lux-users-detail-facts">
              {facts.map((fact) => <span key={fact}>{fact}</span>)}
            </p>
            <div className="lux-users-tags lux-users-detail-tags">
              {[
                user.canRemoteAccess ? "允许远程访问" : "仅本地访问",
                user.canDownload ? "允许下载" : "禁止下载",
                user.presetSource ? `预设：${user.presetSource}` : null,
                (user.currentPlaybackCount ?? 0) > 0 ? `播放中 ${user.currentPlaybackCount} 路` : null,
              ].filter((tag): tag is string => Boolean(tag)).map((tag) => <span key={tag} className="lux-users-tag">{tag}</span>)}
            </div>
          </div>
        </div>
      </header>
      <nav className="lux-users-segments" aria-label="用户详情分区">
        {detailSections.map((item) => (
          <button key={item.id} type="button" aria-current={item.id === sectionId ? "page" : undefined} onClick={() => setSearchParams({ section: item.id })}>
            {item.label}
          </button>
        ))}
      </nav>
      <div className="lux-users-detail-body">
        {sectionId === "overview" ? (
          <>
            <AdminUserProfileEditor key={`profile-${user.id}`} user={user} />
            <AdminUserLifecycleActions key={`lifecycle-${user.id}`} user={user} />
            <AdminUserPasswordPanel key={`password-${user.id}`} user={user} />
          </>
        ) : sectionId === "access" ? (
          <>
            <AdminUserPermissionsSection key={`permissions-${user.id}`} user={user} />
            <AdminUserPlaybackPolicySection key={`playback-${user.id}`} user={user} />
            <AdminUserContentRestrictionSection user={user} />
          </>
        ) : sectionId === "devices" ? (
          <AdminUserSessionsSection key={`devices-${user.id}`} user={user} />
        ) : sectionId === "activity" ? (
          <AdminUserActivityPanel key={`activity-${user.id}`} user={user} />
        ) : (
          <AdminUserPreferencesSection key={`preferences-${user.id}`} user={user} />
        )}
      </div>
    </div>
  );
}

function avatarInitial(name: string): string {
  return name.trim().slice(0, 1).toUpperCase() || "?";
}

const policyGroups = [
  { id: "registration", sections: ["registration", "mail"] },
  { id: "defaults", sections: ["defaults", "presets"] },
  { id: "blocklist", sections: ["blocklist", "proxy"] },
  { id: "retention", sections: ["retention"] },
] as const;

type PolicyGroupId = (typeof policyGroups)[number]["id"];

export function AdminUserSettingsPage() {
  const [searchParams] = useSearchParams();
  const requested = searchParams.get("section") ?? "registration";
  const activeSection = (["registration", "mail", "defaults", "presets", "blocklist", "proxy", "retention"].includes(requested) ? requested : "registration") as PolicySectionKey;
  const groupId: PolicyGroupId = policyGroups.find((group) => (group.sections as readonly string[]).includes(requested))?.id ?? "registration";

  return (
    <div className="lux-admin-page lux-users-page">
      <UsersWorkspace activeSection={activeSection}>
        <div className="lux-users-policy-body">
          {groupId === "registration" ? (
            <>
              <AdminUserRegistrationSettings />
              <AdminUserMailSettings />
            </>
          ) : groupId === "defaults" ? (
            <>
              <AdminUserPermissionPresetsSettings />
              <AdminUserDefaultPermissionsSettings />
            </>
          ) : groupId === "blocklist" ? (
            <>
              <AdminUserGlobalAccessRulesPanel />
              <TrustedProxyPlaceholder />
            </>
          ) : (
            <AdminUserRetentionSettings />
          )}
        </div>
      </UsersWorkspace>
    </div>
  );
}

function TrustedProxyPlaceholder() {
  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-proxy-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-proxy-title">可信代理</h2></div>
        <span className="lux-user-badge is-warn">尚未开放</span>
      </div>
      <p className="lux-admin-muted">只有来自可信代理的连接才会采用 X-Forwarded-For 判断来源 IP。Docker 部署会自动信任容器运行时的默认网关，无需手工填写；此处用于补充额外的前置反向代理地址或网段，界面开放前可经管理员设置接口配置。</p>
      <div className="lux-admin-user-profile-actions">
        <LuxButton tone="secondary" type="button" disabled>保存可信代理</LuxButton>
      </div>
    </section>
  );
}

const adminRegistrationSettingsQueryKey = ["admin", "settings", "registration"] as const;

function AdminUserRegistrationSettings() {
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

function AdminUserMailSettings() {
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

function AdminUserRetentionSettings() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const [graceDays, setGraceDays] = useState("7");
  const [notice, setNotice] = useState("");
  const update = useMutation({
    mutationFn: (days: number) => api.updateAdminSettings({ userDeletionGraceDays: days }),
    onSuccess: (updated, days) => {
      const savedDays = updated.userDeletionGraceDays ?? days;
      queryClient.setQueryData(queryKeys.adminSettings, { ...updated, userDeletionGraceDays: savedDays });
      setGraceDays(String(savedDays));
      setNotice("删除宽限期已保存。");
    },
    onError: (error) => setNotice(error.message || "保存失败，请重试。"),
  });
  useEffect(() => {
    setGraceDays(String(settings.data?.userDeletionGraceDays ?? 7));
  }, [settings.data?.userDeletionGraceDays]);

  if (settings.isPending) return <p className="lux-admin-muted" role="status">正在读取删除保留设置…</p>;
  if (settings.error) return <p className="lux-error-copy" role="alert">读取删除保留设置失败：{settings.error.message}</p>;

  const parsedDays = Number(graceDays);
  const validDays = Number.isInteger(parsedDays) && parsedDays >= 1 && parsedDays <= 3_650;
  const hasChanges = parsedDays !== (settings.data.userDeletionGraceDays ?? 7);

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!validDays || !hasChanges || update.isPending) return;
    setNotice("");
    update.mutate(parsedDays);
  }

  return (
    <>
      <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-retention-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-retention-title">删除账号保留期限</h2></div></div>
        <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit}>
          <label>
            移入回收站后保留天数
            <LuxTextField
              type="number"
              min={1}
              max={3_650}
              step={1}
              value={graceDays}
              aria-label="删除账号保留天数"
              onChange={(event) => { setGraceDays(event.target.value); setNotice(""); }}
              disabled={update.isPending}
            />
          </label>
          <p className="lux-admin-muted">范围为 1–3650 天。新值只影响之后移入回收站的账号；已在回收站中的账号仍按原计划清理时间处理。</p>
          <div className="lux-admin-user-profile-actions">
            <LuxButton tone="primary" type="submit" disabled={!validDays || !hasChanges || update.isPending}>
              {update.isPending ? "保存中…" : "保存保留期限"}
            </LuxButton>
            {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
          </div>
        </form>
      </section>
      <AdminUserActivityRetentionSettings />
    </>
  );
}

function AdminUserActivityRetentionSettings() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const [retentionDays, setRetentionDays] = useState("90");
  const [notice, setNotice] = useState("");
  const update = useMutation({
    mutationFn: (days: number) => api.updateAdminSettings({ userActivityRetentionDays: days }),
    onSuccess: (updated, days) => {
      const savedDays = updated.userActivityRetentionDays ?? days;
      queryClient.setQueryData(queryKeys.adminSettings, { ...updated, userActivityRetentionDays: savedDays });
      setRetentionDays(String(savedDays));
      setNotice("活动记录保留期限已保存。");
    },
    onError: (error) => setNotice(error.message || "保存失败，请重试。"),
  });
  useEffect(() => {
    setRetentionDays(String(settings.data?.userActivityRetentionDays ?? 90));
  }, [settings.data?.userActivityRetentionDays]);

  if (settings.isPending) return <p className="lux-admin-muted" role="status">正在读取活动记录保留设置…</p>;
  if (settings.error) return <p className="lux-error-copy" role="alert">读取活动记录保留设置失败：{settings.error.message}</p>;

  const parsedDays = Number(retentionDays);
  const validDays = Number.isInteger(parsedDays) && parsedDays >= 1 && parsedDays <= 3_650;
  const hasChanges = parsedDays !== (settings.data.userActivityRetentionDays ?? 90);

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!validDays || !hasChanges || update.isPending) return;
    setNotice("");
    update.mutate(parsedDays);
  }

  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-activity-retention-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-activity-retention-title">活动记录保留</h2></div></div>
      <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit}>
        <label>
          活动记录保留天数
          <LuxTextField
            type="number"
            min={1}
            max={3_650}
            step={1}
            value={retentionDays}
            aria-label="活动记录保留天数"
            onChange={(event) => { setRetentionDays(event.target.value); setNotice(""); }}
            disabled={update.isPending}
          />
        </label>
        <p className="lux-admin-muted">范围为 1–3650 天。超过期限的用户活动索引会由后台定期清理；观看进度、已看状态、收藏和文件审计不受影响。</p>
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="primary" type="submit" disabled={!validDays || !hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存活动保留期限"}
          </LuxButton>
          {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

function AdminUserDefaultPermissionsSettings() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const librariesQuery = useQuery({ queryKey: queryKeys.adminLibraries, queryFn: () => api.adminLibraries() });
  const [draft, setDraft] = useState<NewUserPermissions | null>(null);
  const [limitDraft, setLimitDraft] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const current = settings.data?.newUserPermissions;
  const currentDraft = draft ?? current;
  const libraries = librariesQuery.data?.libraries ?? [];
  const enabledLibraries = libraries.filter((library) => library.isEnabled);
  const enabledLibraryIds = new Set(enabledLibraries.map((library) => library.id));
  const selectedLibraryIds = currentDraft?.libraryIds.filter((id) => enabledLibraryIds.has(id)) ?? [];
  const unavailableLibraryNames = currentDraft?.libraryIds
    .filter((id) => !enabledLibraryIds.has(id))
    .map((id) => libraries.find((library) => library.id === id)?.name ?? id) ?? [];
  const modeForSave: AdminUserLibraryAccessMode = currentDraft?.libraryAccessMode === "SELECTED" && selectedLibraryIds.length === 0
    ? "NONE"
    : currentDraft?.libraryAccessMode ?? "NONE";
  const libraryIdsForSave = modeForSave === "SELECTED" ? selectedLibraryIds : [];
  const limitInput = limitDraft ?? (currentDraft ? String(currentDraft.maxSimultaneousPlaybacks) : "");
  const parsedLimit = Number(limitInput);
  const validLimit = limitInput.trim() !== "" && Number.isSafeInteger(parsedLimit) && parsedLimit >= 0;
  const updatedDraft = currentDraft && validLimit ? {
    ...currentDraft,
    libraryAccessMode: modeForSave,
    libraryIds: libraryIdsForSave,
    maxSimultaneousPlaybacks: parsedLimit,
  } : null;
  const hasChanges = current != null && updatedDraft != null && !sameNewUserPermissions(updatedDraft, current);
  const update = useMutation({
    mutationFn: (permissions: NewUserPermissions) => api.updateAdminSettings({ newUserPermissions: permissions }),
    onSuccess: (updated, submitted) => {
      const saved = updated.newUserPermissions ?? submitted;
      queryClient.setQueryData<AdminSettings>(queryKeys.adminSettings, (previous) => ({
        ...previous,
        ...updated,
        newUserPermissions: saved,
      } as AdminSettings));
      setDraft(saved);
      setLimitDraft(String(saved.maxSimultaneousPlaybacks));
      setError("");
      setNotice("新用户默认权限已保存。");
    },
    onError: (cause) => {
      setNotice("");
      setError(cause.message || "新用户默认权限保存失败，请重试。");
    },
  });

  useEffect(() => {
    if (!current) return;
    setDraft(current);
    setLimitDraft(String(current.maxSimultaneousPlaybacks));
  }, [current]);

  if (settings.isPending || librariesQuery.isPending) {
    return <AdminUserManagementState label="正在读取新用户默认权限…" />;
  }
  if (settings.error) {
    return <AdminUserManagementState label={`读取新用户默认权限失败：${settings.error.message}`} error />;
  }
  if (librariesQuery.error) {
    return <AdminUserManagementState label={`读取媒体库列表失败：${librariesQuery.error.message}`} error />;
  }
  if (!current || !currentDraft) {
    return <AdminUserManagementState label="服务端没有返回新用户默认权限，未展示本地默认值。" error />;
  }

  function updateDraft(change: Partial<NewUserPermissions>) {
    if (!currentDraft) return;
    setDraft({ ...currentDraft, ...change });
    setNotice("");
    setError("");
  }

  function toggleLibrary(libraryId: string, checked: boolean) {
    const next = new Set(selectedLibraryIds);
    if (checked) next.add(libraryId);
    else next.delete(libraryId);
    updateDraft({ libraryIds: [...next] });
  }

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!updatedDraft || !validLimit || !hasChanges || update.isPending) return;
    setNotice("");
    setError("");
    update.mutate(updatedDraft);
  }

  return (
    <section className="lux-admin-panel lux-admin-user-permissions lux-admin-new-user-permissions" aria-labelledby="lux-admin-new-user-permissions-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-new-user-permissions-title">新用户默认权限</h2></div></div>
      <form className="lux-admin-form lux-admin-user-defaults-form" onSubmit={submit}>
        <fieldset className="lux-admin-user-access-modes">
          <legend>媒体库访问范围</legend>
          {([
            ["ALL", "全部媒体库", "允许访问所有已启用媒体库，包括之后新增的媒体库。"],
            ["SELECTED", "指定媒体库", "仅允许访问下方勾选的媒体库。"],
            ["NONE", "无媒体库权限", "用户不能浏览、搜索或播放任何媒体库内容。"],
          ] as const).map(([mode, title, description]) => (
            <LuxRadio key={mode} name="new-user-library-access" value={mode} checked={currentDraft.libraryAccessMode === mode} disabled={update.isPending} onChange={() => updateDraft({ libraryAccessMode: mode })} label=<><strong>{title}</strong><small>{description}</small></> />
          ))}
        </fieldset>
        {currentDraft.libraryAccessMode === "ALL" ? <p className="lux-admin-muted">新用户可访问所有已启用媒体库，包括之后新增的媒体库。</p> : null}
        {currentDraft.libraryAccessMode === "NONE" ? <p className="lux-admin-muted">新用户不能访问任何媒体库。</p> : null}
        {currentDraft.libraryAccessMode === "SELECTED" ? (
          <>
            {unavailableLibraryNames.length ? <p className="lux-admin-muted">已停用或不可用媒体库将从默认权限移除：{unavailableLibraryNames.join("、")}。</p> : null}
            <p className="lux-admin-muted">
              {enabledLibraries.length
                ? selectedLibraryIds.length
                  ? `已选择 ${selectedLibraryIds.length} 个媒体库。`
                  : "请至少选择一个媒体库；空选择保存后会设为无媒体库权限。"
                : "当前没有已启用的媒体库；空选择保存后会设为无媒体库权限。"}
            </p>
            {enabledLibraries.length ? (
              <div className="lux-admin-library-access">
                {enabledLibraries.map((library) => (
                  <LuxCheckbox key={library.id} value={library.id} checked={selectedLibraryIds.includes(library.id)} disabled={update.isPending} onChange={(event) => toggleLibrary(library.id, event.currentTarget.checked)} label=<>{library.name}</> />
                ))}
              </div>
            ) : null}
          </>
        ) : null}
        <fieldset className="lux-admin-user-defaults-policy-options">
          <legend>下载与播放/转码</legend>
          <LuxCheckbox aria-label="允许下载" checked={currentDraft.canDownload} disabled={update.isPending} onChange={(event) => updateDraft({ canDownload: event.currentTarget.checked })} label="允许下载" />
          <LuxCheckbox aria-label="允许播放" checked={currentDraft.canPlay} disabled={update.isPending} onChange={(event) => updateDraft({ canPlay: event.currentTarget.checked })} label="允许播放" />
          <LuxCheckbox aria-label="允许封装转换" checked={currentDraft.allowRemux} disabled={update.isPending} onChange={(event) => updateDraft({ allowRemux: event.currentTarget.checked })} label="允许封装转换" />
          <LuxCheckbox aria-label="允许音频转码" checked={currentDraft.allowAudioTranscode} disabled={update.isPending} onChange={(event) => updateDraft({ allowAudioTranscode: event.currentTarget.checked })} label="允许音频转码" />
          <LuxCheckbox aria-label="允许视频转码" checked={currentDraft.allowVideoTranscode} disabled={update.isPending} onChange={(event) => updateDraft({ allowVideoTranscode: event.currentTarget.checked })} label="允许视频转码" />
        </fieldset>
        <label className="lux-admin-user-playback-limit">
          同时播放上限（0 表示不限）
          <LuxTextField type="number" aria-label="新用户同时播放上限" min={0} step={1} value={limitInput} disabled={update.isPending} onChange={(event) => { setLimitDraft(event.currentTarget.value); setNotice(""); setError(""); }} />
        </label>
        {!validLimit ? <p className="lux-admin-user-playback-validation">请输入大于或等于 0 的整数。</p> : null}
        <p className="lux-admin-muted">此处配置新账号的初始权限快照；修改不会自动改变已有用户。</p>
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="primary" type="submit" disabled={!validLimit || !hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存默认权限"}
          </LuxButton>
          {error ? <span role="alert">{error}</span> : notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

function AdminUserPermissionPresetsSettings() {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const queryKey = ["admin", "user-permission-presets"] as const;
  const presetsQuery = useQuery({ queryKey, queryFn: () => api.adminUserPermissionPresets() });
  const librariesQuery = useQuery({ queryKey: queryKeys.adminLibraries, queryFn: () => api.adminLibraries() });
  const [editingPreset, setEditingPreset] = useState<AdminUserPermissionPreset | null>(null);
  const [draft, setDraft] = useState<AdminUserPermissionPresetInput | null>(null);
  const [limitDraft, setLimitDraft] = useState("2");
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const presets = presetsQuery.data?.presets ?? [];
  const enabledLibraries = (librariesQuery.data?.libraries ?? []).filter((library) => library.isEnabled);
  const enabledLibraryIds = new Set(enabledLibraries.map((library) => library.id));
  const selectedLibraryIds = draft?.permissions.libraryIds.filter((id) => enabledLibraryIds.has(id)) ?? [];
  const limit = Number(limitDraft);
  const validLimit = limitDraft.trim() !== "" && Number.isSafeInteger(limit) && limit >= 0;
  const validName = Boolean(draft?.name.trim()) && (draft?.name.trim().length ?? 0) <= 80;
  const validDescription = (draft?.description?.length ?? 0) <= 500;
  const modeForSave: AdminUserLibraryAccessMode = draft?.permissions.libraryAccessMode === "SELECTED" && selectedLibraryIds.length === 0
    ? "NONE"
    : draft?.permissions.libraryAccessMode ?? "NONE";
  const canSave = Boolean(draft && validName && validDescription && validLimit);

  const finishMutation = (message: string) => {
    setDraft(null);
    setEditingPreset(null);
    setError("");
    setNotice(message);
    void queryClient.invalidateQueries({ queryKey });
  };
  const showMutationError = (cause: Error) => {
    setNotice("");
    setError(cause.message || "角色预设操作失败，请重试。");
  };
  const create = useMutation({
    mutationFn: (input: AdminUserPermissionPresetInput) => api.createAdminUserPermissionPreset(input),
    onSuccess: () => finishMutation("角色预设已创建。"),
    onError: showMutationError,
  });
  const update = useMutation({
    mutationFn: ({ presetId, version, input }: { presetId: string; version: number; input: AdminUserPermissionPresetInput }) =>
      api.updateAdminUserPermissionPreset(presetId, version, input),
    onSuccess: () => finishMutation("角色预设已更新。"),
    onError: showMutationError,
  });
  const remove = useMutation({
    mutationFn: ({ presetId, version }: { presetId: string; version: number }) =>
      api.deleteAdminUserPermissionPreset(presetId, version),
    onSuccess: () => finishMutation("角色预设已删除。"),
    onError: showMutationError,
  });
  const busy = create.isPending || update.isPending || remove.isPending;

  function startCreate() {
    setEditingPreset(null);
    setDraft({
      name: "",
      description: null,
      isAdmin: false,
      canManageServer: false,
      permissions: {
        libraryAccessMode: "NONE",
        libraryIds: [],
        canDownload: false,
        canPlay: true,
        maxSimultaneousPlaybacks: 2,
        allowRemux: true,
        allowAudioTranscode: true,
        allowVideoTranscode: true,
      },
    });
    setLimitDraft("2");
    setError("");
    setNotice("");
  }

  function startEdit(preset: AdminUserPermissionPreset) {
    setEditingPreset(preset);
    setDraft({
      name: preset.name,
      description: preset.description,
      isAdmin: preset.isAdmin,
      canManageServer: preset.canManageServer,
      permissions: { ...preset.permissions, libraryIds: [...preset.permissions.libraryIds] },
    });
    setLimitDraft(String(preset.permissions.maxSimultaneousPlaybacks));
    setError("");
    setNotice("");
  }

  function updateDraft(change: Partial<AdminUserPermissionPresetInput>) {
    setDraft((current) => current ? {
      ...current,
      ...change,
      canManageServer: change.isAdmin === false ? false : change.canManageServer ?? current.canManageServer,
    } : current);
    setError("");
    setNotice("");
  }

  function updatePermissionDraft(change: Partial<NewUserPermissions>) {
    setDraft((current) => current ? {
      ...current,
      permissions: { ...current.permissions, ...change },
    } : current);
    setError("");
    setNotice("");
  }

  function toggleLibrary(libraryId: string, checked: boolean) {
    const next = new Set(selectedLibraryIds);
    if (checked) next.add(libraryId);
    else next.delete(libraryId);
    updatePermissionDraft({ libraryIds: [...next] });
  }

  function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!draft || !canSave || busy) return;
    const permissions: NewUserPermissions = {
      ...draft.permissions,
      libraryAccessMode: modeForSave,
      libraryIds: modeForSave === "SELECTED" ? selectedLibraryIds : [],
      maxSimultaneousPlaybacks: limit,
    };
    const input: AdminUserPermissionPresetInput = {
      ...draft,
      name: draft.name.trim(),
      description: draft.description?.trim() || null,
      canManageServer: draft.isAdmin && draft.canManageServer,
      permissions,
    };
    setError("");
    setNotice("");
    if (editingPreset) {
      update.mutate({ presetId: editingPreset.id, version: editingPreset.version, input });
    } else {
      create.mutate(input);
    }
  }

  async function deletePreset(preset: AdminUserPermissionPreset) {
    if (busy || preset.isBuiltin) return;
    const accepted = await confirm(`确定删除“${preset.name}”预设吗？已应用到用户的权限快照不会改变。`);
    if (!accepted) return;
    setError("");
    setNotice("");
    remove.mutate({ presetId: preset.id, version: preset.version });
  }

  if (presetsQuery.isPending || librariesQuery.isPending) {
    return <AdminUserManagementState label="正在读取角色预设…" />;
  }
  if (presetsQuery.error) {
    return <AdminUserManagementState label={`读取角色预设失败：${presetsQuery.error.message}`} error />;
  }
  if (librariesQuery.error) {
    return <AdminUserManagementState label={`读取媒体库列表失败：${librariesQuery.error.message}`} error />;
  }

  return (
    <section className="lux-admin-panel lux-admin-user-permissions lux-admin-user-presets" aria-labelledby="lux-admin-user-presets-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-presets-title">角色预设</h2><p className="lux-admin-muted">预设保存权限快照；之后修改不会改变已应用到用户的权限。</p></div>
        <LuxButton tone="primary" type="button" onClick={startCreate} disabled={busy}>新建角色预设</LuxButton>
      </div>
      <div className="lux-admin-user-defaults-form">
        {presets.map((preset) => {
          const librarySummary = preset.permissions.libraryAccessMode === "ALL"
            ? "全部媒体库"
            : preset.permissions.libraryAccessMode === "NONE"
              ? "无媒体库"
              : `指定 ${preset.permissions.libraryIds.length} 个媒体库`;
          return (
            <article className="lux-admin-panel" key={preset.id}>
              <div className="lux-admin-panel-heading">
                <div><h3>{preset.name}</h3><p className="lux-admin-muted">{preset.isBuiltin ? "内置预设" : `自定义预设 · v${preset.version}`}</p></div>
                <div className="lux-admin-user-profile-actions">
                  <LuxButton tone="secondary" type="button" aria-label={`编辑预设 ${preset.name}`} onClick={() => startEdit(preset)} disabled={busy || preset.isBuiltin}>编辑</LuxButton>
                  <LuxButton tone="secondary" type="button" aria-label={`删除预设 ${preset.name}`} onClick={() => deletePreset(preset)} disabled={busy || preset.isBuiltin}>删除</LuxButton>
                </div>
              </div>
              <p className="lux-admin-muted">{preset.description || "未填写说明"} · {preset.isAdmin ? "管理员" : "普通用户"}{preset.canManageServer ? "，可进入管理台" : ""}</p>
              <p className="lux-admin-muted">{librarySummary} · {preset.permissions.canDownload ? "允许下载" : "禁止下载"} · {preset.permissions.canPlay ? "允许播放" : "禁止播放"} · 并发 {preset.permissions.maxSimultaneousPlaybacks === 0 ? "不限" : preset.permissions.maxSimultaneousPlaybacks}</p>
              <p className="lux-admin-muted">封装转换 {preset.permissions.allowRemux ? "允许" : "禁止"} · 音频转码 {preset.permissions.allowAudioTranscode ? "允许" : "禁止"} · 视频转码 {preset.permissions.allowVideoTranscode ? "允许" : "禁止"}</p>
            </article>
          );
        })}
      </div>
      {draft ? (
        <form className="lux-admin-form lux-admin-user-defaults-form" onSubmit={save}>
          <div className="lux-admin-panel-heading"><div><h3>{editingPreset ? `编辑预设：${editingPreset.name}` : "新建角色预设"}</h3></div></div>
          <label>预设名称<LuxTextField aria-label="预设名称" maxLength={80} value={draft.name} disabled={busy} onChange={(event) => updateDraft({ name: event.currentTarget.value })} /></label>
          <label>说明<LuxTextField aria-label="预设说明" maxLength={500} value={draft.description ?? ""} disabled={busy} onChange={(event) => updateDraft({ description: event.currentTarget.value || null })} /></label>
          <fieldset className="lux-admin-user-defaults-policy-options">
            <legend>账号角色</legend>
            <LuxCheckbox aria-label="管理员角色" checked={draft.isAdmin} disabled={busy} onChange={(event) => updateDraft({ isAdmin: event.currentTarget.checked })} label="管理员角色" />
            <LuxCheckbox aria-label="允许进入管理台" checked={draft.canManageServer} disabled={busy || !draft.isAdmin} onChange={(event) => updateDraft({ canManageServer: event.currentTarget.checked })} label="允许进入管理台" />
          </fieldset>
          <fieldset className="lux-admin-user-access-modes">
            <legend>媒体库访问范围</legend>
            {([
              ["ALL", "全部媒体库"],
              ["SELECTED", "指定媒体库"],
              ["NONE", "无媒体库权限"],
            ] as const).map(([mode, title]) => (
              <LuxRadio key={mode} name="preset-library-access" value={mode} checked={draft.permissions.libraryAccessMode === mode} disabled={busy} onChange={() => updatePermissionDraft({ libraryAccessMode: mode, libraryIds: mode === "SELECTED" ? draft.permissions.libraryIds : [] })} label=<><strong>{title}</strong></> />
            ))}
          </fieldset>
          {draft.permissions.libraryAccessMode === "SELECTED" ? (
            <>
              {enabledLibraries.length ? (
                <div className="lux-admin-library-access">
                  {enabledLibraries.map((library) => (
                    <LuxCheckbox key={library.id} checked={selectedLibraryIds.includes(library.id)} disabled={busy} onChange={(event) => toggleLibrary(library.id, event.currentTarget.checked)} label=<>{library.name}</> />
                  ))}
                </div>
              ) : <p className="lux-admin-muted">当前没有已启用的媒体库；空选择保存后会设为无媒体库权限。</p>}
              {enabledLibraries.length > 0 && selectedLibraryIds.length === 0 ? <p className="lux-admin-muted">空选择保存后会设为无媒体库权限。</p> : null}
            </>
          ) : null}
          <fieldset className="lux-admin-user-defaults-policy-options">
            <legend>下载与播放/转码</legend>
            <LuxCheckbox aria-label="允许下载" checked={draft.permissions.canDownload} disabled={busy} onChange={(event) => updatePermissionDraft({ canDownload: event.currentTarget.checked })} label="允许下载" />
            <LuxCheckbox aria-label="允许播放" checked={draft.permissions.canPlay} disabled={busy} onChange={(event) => updatePermissionDraft({ canPlay: event.currentTarget.checked })} label="允许播放" />
            <LuxCheckbox aria-label="允许封装转换" checked={draft.permissions.allowRemux} disabled={busy} onChange={(event) => updatePermissionDraft({ allowRemux: event.currentTarget.checked })} label="允许封装转换" />
            <LuxCheckbox aria-label="允许音频转码" checked={draft.permissions.allowAudioTranscode} disabled={busy} onChange={(event) => updatePermissionDraft({ allowAudioTranscode: event.currentTarget.checked })} label="允许音频转码" />
            <LuxCheckbox aria-label="允许视频转码" checked={draft.permissions.allowVideoTranscode} disabled={busy} onChange={(event) => updatePermissionDraft({ allowVideoTranscode: event.currentTarget.checked })} label="允许视频转码" />
          </fieldset>
          <label className="lux-admin-user-playback-limit">同时播放上限（0 表示不限）<LuxTextField type="number" aria-label="预设同时播放上限" min={0} step={1} value={limitDraft} disabled={busy} onChange={(event) => { setLimitDraft(event.currentTarget.value); setError(""); setNotice(""); }} /></label>
          {!validName ? <p className="lux-admin-user-playback-validation">请输入 1–80 个字符的预设名称。</p> : null}
          {!validDescription ? <p className="lux-admin-user-playback-validation">说明不能超过 500 个字符。</p> : null}
          {!validLimit ? <p className="lux-admin-user-playback-validation">请输入大于或等于 0 的整数。</p> : null}
          <p className="lux-admin-muted">应用预设时会复制权限快照；以后修改预设不会自动改变已有用户。</p>
          <div className="lux-admin-user-profile-actions">
            <LuxButton tone="primary" type="submit" disabled={!canSave || busy}>{busy ? "保存中…" : "保存预设"}</LuxButton>
            <LuxButton tone="secondary" type="button" disabled={busy} onClick={() => { setDraft(null); setEditingPreset(null); setError(""); }}>取消</LuxButton>
            {error ? <span role="alert">{error}</span> : notice ? <span role="status">{notice}</span> : null}
          </div>
        </form>
      ) : notice ? <p role="status">{notice}</p> : error ? <p role="alert">{error}</p> : null}
    </section>
  );
}

function sameNewUserPermissions(left: NewUserPermissions, right: NewUserPermissions) {
  return left.libraryAccessMode === right.libraryAccessMode
    && left.libraryIds.length === right.libraryIds.length
    && left.libraryIds.every((id, index) => id === right.libraryIds[index])
    && left.canDownload === right.canDownload
    && left.canPlay === right.canPlay
    && left.maxSimultaneousPlaybacks === right.maxSimultaneousPlaybacks
    && left.allowRemux === right.allowRemux
    && left.allowAudioTranscode === right.allowAudioTranscode
    && left.allowVideoTranscode === right.allowVideoTranscode;
}
