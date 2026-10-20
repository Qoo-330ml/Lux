import { ArrowLeft, CircleDashed, ShieldCheck, SlidersHorizontal, UserRound, Users } from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ApiError, api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminMailSettingsResponse, AdminRegistrationSettings, RegistrationMode, AdminSettings, AdminUser, AdminUserActivityEvent, AdminUserContentRestrictionPolicy, AdminUserContentRestrictionUpdateInput, AdminUserEmbySession, AdminUserLibraryAccessMode, AdminUserPermissionPreset, AdminUserPermissionPresetInput, AdminUserPlaybackPolicy, AdminUserPlaybackSession, AdminUserWebSession, MailSettings, NewUserPermissions, SmtpTlsMode } from "../../lib/api/types";
import { AdminUserAccessRulesPanel } from "./AdminUserAccessRulesPanel";
import { AdminUserGlobalAccessRulesPanel } from "./AdminUserGlobalAccessRulesPanel";
import "./user-management.css";

const detailSections = [
  { id: "profile", label: "资料与状态", module: "模块一", icon: Users },
  { id: "permissions", label: "权限", module: "模块四", icon: ShieldCheck },
  { id: "preferences", label: "偏好", module: "模块四", icon: SlidersHorizontal },
  { id: "content", label: "内容限制", module: "模块五", icon: ShieldCheck },
  { id: "devices", label: "设备与登录", module: "模块七", icon: Users },
  { id: "playback", label: "播放与转码", module: "模块六", icon: CircleDashed },
  { id: "activity", label: "活动记录", module: "模块七", icon: CircleDashed },
] as const;

const settingsSections = [
  { id: "registration", label: "注册与邀请", module: "模块二" },
  { id: "mail", label: "邮件", module: "模块二" },
  { id: "defaults", label: "新用户默认权限", module: "模块四" },
  { id: "presets", label: "角色预设", module: "模块四" },
  { id: "blocklist", label: "全站黑名单", module: "模块七" },
  { id: "retention", label: "删除与活动保留", module: "模块一 / 七" },
  { id: "proxy", label: "可信代理", module: "模块七" },
] as const;

const contentRatingLevels = {
  MPA: ["G", "PG", "PG-13", "R", "NC-17"],
  TVPG: ["TV-Y", "TV-G", "TV-Y7", "TV-PG", "TV-14", "TV-MA"],
  BBFC: ["U", "PG", "12", "12A", "15", "18", "R18"],
} as const;

type ContentRestrictionFormDraft = Pick<
  AdminUserContentRestrictionPolicy,
  "ratingSystem" | "maximumRating" | "allowUnrated"
> & {
  allowedTagsText: string;
  blockedTagsText: string;
};

export function AdminUserDetailPage() {
  const { userId = "" } = useParams();
  const [searchParams, setSearchParams] = useSearchParams();
  const sectionId = searchParams.get("section") ?? "profile";
  const section = detailSections.find((item) => item.id === sectionId) ?? detailSections[0];
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
  const Icon = section.icon;

  return (
    <div className="lux-admin-page lux-admin-user-management-page">
      <Link className="lux-admin-user-back" to="/admin/users"><ArrowLeft size={15} />返回用户列表</Link>
      <header className="lux-admin-user-page-heading">
        <div>
          <span className="lux-admin-eyebrow">{user.usernameNormalized}</span>
          <h1>{displayName}</h1>
          <p>用户详情与访问管理</p>
        </div>
        <span className={user.deletedAt != null || user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>{statusLabel}</span>
      </header>
      <nav className="lux-admin-user-section-nav" aria-label="用户详情模块">
        {detailSections.map((item) => (
          <button key={item.id} className={item.id === section.id ? "is-active" : undefined} type="button" aria-current={item.id === section.id ? "page" : undefined} onClick={() => setSearchParams({ section: item.id })}>
            <span>{item.label}</span><small>{item.module}</small>
          </button>
        ))}
      </nav>
      {section.id === "profile" ? (
        <>
          <AdminUserProfileEditor key={user.id} user={user} />
          <AdminUserLifecycleActions user={user} />
        </>
      ) : section.id === "permissions" ? (
        <AdminUserPermissionsSection key={user.id} user={user} />
      ) : section.id === "content" ? (
        <AdminUserContentRestrictionSection user={user} />
      ) : section.id === "playback" ? (
        <AdminUserPlaybackPolicySection key={user.id} user={user} />
      ) : section.id === "devices" ? (
        <AdminUserSessionsSection key={user.id} user={user} />
      ) : section.id === "activity" ? (
        <AdminUserActivityPanel key={user.id} user={user} />
      ) : (
        <section className="lux-admin-user-module-placeholder" aria-labelledby="lux-admin-user-module-title">
          <span className="lux-admin-user-module-icon"><Icon size={19} /></span>
          <div>
            <span className="lux-admin-eyebrow">{section.module}</span>
            <h2 id="lux-admin-user-module-title">{section.label}</h2>
            <p>页面结构已预留。此模块将在对应的用户管理任务中接入服务端数据和操作。</p>
          </div>
        </section>
      )}
    </div>
  );
}

function AdminUserContentRestrictionSection({ user }: { user: AdminUser }) {
  return (
    <div className="lux-admin-user-content-section">
      <AdminUserContentRestrictionEditor key={user.id} user={user} />
      <AdminUserContentIndexStatus />
    </div>
  );
}

function AdminUserContentRestrictionEditor({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const queryKey = ["admin", "user", user.id, "content-restrictions"];
  const policyQuery = useQuery({
    queryKey,
    queryFn: () => api.adminUserContentRestrictions(user.id),
    enabled: user.deletedAt == null,
  });
  const [draft, setDraft] = useState<ContentRestrictionFormDraft | null>(null);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const [versionConflict, setVersionConflict] = useState(false);
  const currentPolicy = policyQuery.data?.policy ?? null;
  const baselineDraft = contentRestrictionFormDraft(currentPolicy);
  const currentDraft = draft ?? baselineDraft;
  const hasChanges = !sameContentRestrictionDraft(currentDraft, baselineDraft);
  const save = useMutation({
    mutationFn: (input: AdminUserContentRestrictionUpdateInput) =>
      api.updateAdminUserContentRestrictions(user.id, input),
    onSuccess: ({ policy }) => {
      setDraft(contentRestrictionFormDraft(policy));
      setError("");
      setVersionConflict(false);
      setNotice("内容限制已保存。");
      queryClient.setQueryData(queryKey, { policy });
    },
    onError: (cause) => {
      setNotice("");
      if (cause instanceof ApiError && cause.status === 409) {
        setVersionConflict(true);
        setError("策略已被其他管理员更改。请重新读取最新策略后再保存。");
        return;
      }
      setError(cause.message || "内容限制保存失败，请重试。");
    },
  });

  if (user.deletedAt != null) {
    return (
      <section className="lux-admin-panel lux-admin-user-content-policy" aria-labelledby="lux-admin-user-content-policy-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-content-policy-title">用户内容限制</h2></div></div>
        <p className="lux-admin-muted" role="status">回收站中的账号内容限制为只读，恢复后可修改。</p>
      </section>
    );
  }

  if (policyQuery.isPending) {
    return (
      <section className="lux-admin-panel lux-admin-user-content-policy" aria-busy="true" aria-labelledby="lux-admin-user-content-policy-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-content-policy-title">用户内容限制</h2></div></div>
        <p className="lux-admin-muted" role="status">正在读取用户内容限制…</p>
      </section>
    );
  }

  if (policyQuery.error || !policyQuery.data) {
    return (
      <section className="lux-admin-panel lux-admin-user-content-policy" aria-labelledby="lux-admin-user-content-policy-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-content-policy-title">用户内容限制</h2></div></div>
        <p className="lux-error-copy" role="alert">读取用户内容限制失败：{policyQuery.error?.message || "没有收到策略数据"}</p>
        <button className="lux-button lux-button-secondary" type="button" onClick={() => void policyQuery.refetch()}>重试</button>
      </section>
    );
  }

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!hasChanges || versionConflict) return;
    setError("");
    setNotice("");
    save.mutate({
      expectedVersion: currentPolicy?.version ?? 0,
      ratingSystem: currentDraft.ratingSystem,
      maximumRating: currentDraft.maximumRating,
      allowUnrated: currentDraft.allowUnrated,
      allowedTags: parseContentRestrictionTags(currentDraft.allowedTagsText),
      blockedTags: parseContentRestrictionTags(currentDraft.blockedTagsText),
    });
  }

  function reloadLatestPolicy() {
    setError("");
    setNotice("");
    void policyQuery.refetch().then((result) => {
      if (result.error || !result.data) {
        setError(`重新读取策略失败：${result.error?.message || "没有收到策略数据"}`);
        return;
      }
      setDraft(contentRestrictionFormDraft(result.data.policy));
      setVersionConflict(false);
      setNotice("已载入最新策略，请检查后再保存。");
    });
  }

  return (
    <section className="lux-admin-panel lux-admin-user-content-policy" aria-labelledby="lux-admin-user-content-policy-title">
      <div className="lux-admin-panel-heading">
        <div><span className="lux-admin-eyebrow">模块五</span><h2 id="lux-admin-user-content-policy-title">用户内容限制</h2></div>
        <span className={`lux-user-badge ${currentPolicy ? "is-ok" : ""}`}>{currentPolicy ? "已配置" : "未配置"}</span>
      </div>
      {currentPolicy ? (
        <p className="lux-admin-muted lux-admin-user-content-policy-version">当前版本 {currentPolicy.version}</p>
      ) : (
        <p className="lux-admin-muted lux-admin-user-content-policy-default" role="status">当前没有已保存的限制；默认不限制分级和标签，并允许未分级内容。</p>
      )}
      <form className="lux-admin-form lux-admin-user-content-policy-form" onSubmit={submit}>
        <div className="lux-admin-user-content-policy-grid">
          <label>
            分级体系
            <select
              aria-label="分级体系"
              value={currentDraft.ratingSystem}
              disabled={save.isPending || versionConflict}
              onChange={(event) => setDraft({
                ...currentDraft,
                ratingSystem: event.currentTarget.value as AdminUserContentRestrictionPolicy["ratingSystem"],
                maximumRating: null,
              })}
            >
              <option value="MPA">MPA 电影分级</option>
              <option value="TVPG">TV 家长指引</option>
              <option value="BBFC">BBFC 英国分级</option>
            </select>
          </label>
          <label>
            最高允许等级
            <select
              aria-label="最高允许等级"
              value={currentDraft.maximumRating ?? ""}
              disabled={save.isPending || versionConflict}
              onChange={(event) => setDraft({ ...currentDraft, maximumRating: event.currentTarget.value || null })}
            >
              <option value="">不限制</option>
              {contentRatingLevels[currentDraft.ratingSystem].map((rating) => <option key={rating} value={rating}>{rating}</option>)}
            </select>
          </label>
        </div>
        <label className="lux-admin-user-content-policy-toggle">
          <input
            type="checkbox"
            aria-label="允许未分级内容"
            checked={currentDraft.allowUnrated}
            disabled={save.isPending || versionConflict}
            onChange={(event) => setDraft({ ...currentDraft, allowUnrated: event.currentTarget.checked })}
          />
          <span>允许未分级内容</span>
        </label>
        <div className="lux-admin-user-content-policy-grid">
          <label>
            允许标签（每行一项）
            <textarea
              aria-label="允许标签"
              rows={4}
              value={currentDraft.allowedTagsText}
              disabled={save.isPending || versionConflict}
              onChange={(event) => setDraft({ ...currentDraft, allowedTagsText: event.currentTarget.value })}
            />
            <span className="lux-admin-muted">填写后，内容至少要命中一个允许标签。</span>
          </label>
          <label>
            禁止标签（每行一项）
            <textarea
              aria-label="禁止标签"
              rows={4}
              value={currentDraft.blockedTagsText}
              disabled={save.isPending || versionConflict}
              onChange={(event) => setDraft({ ...currentDraft, blockedTagsText: event.currentTarget.value })}
            />
            <span className="lux-admin-muted">禁止标签优先，命中后拒绝访问。</span>
          </label>
        </div>
        <p className="lux-admin-muted">标签按媒体分类索引匹配；每组最多 100 项。保存后策略已存储，但媒体访问拦截尚未接入。</p>
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-primary" type="submit" disabled={!hasChanges || save.isPending || versionConflict}>
            {save.isPending ? "保存中…" : "保存内容限制"}
          </button>
          {versionConflict ? <button className="lux-button lux-button-secondary" type="button" onClick={reloadLatestPolicy}>重新读取策略</button> : null}
          {error ? <span role="alert">{error}</span> : null}
          {notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

function contentRestrictionFormDraft(policy: AdminUserContentRestrictionPolicy | null): ContentRestrictionFormDraft {
  return {
    ratingSystem: policy?.ratingSystem ?? "MPA",
    maximumRating: policy?.maximumRating ?? null,
    allowUnrated: policy?.allowUnrated ?? true,
    allowedTagsText: policy?.allowedTags.join("\n") ?? "",
    blockedTagsText: policy?.blockedTags.join("\n") ?? "",
  };
}

function sameContentRestrictionDraft(left: ContentRestrictionFormDraft, right: ContentRestrictionFormDraft) {
  return left.ratingSystem === right.ratingSystem
    && left.maximumRating === right.maximumRating
    && left.allowUnrated === right.allowUnrated
    && left.allowedTagsText === right.allowedTagsText
    && left.blockedTagsText === right.blockedTagsText;
}

function parseContentRestrictionTags(value: string) {
  return value.split(/\r?\n/).map((tag) => tag.trim()).filter(Boolean);
}

function AdminUserContentIndexStatus() {
  const readiness = useQuery({
    queryKey: ["admin", "content-restriction-index"],
    queryFn: () => api.adminContentRestrictionIndexStatus(),
    refetchInterval: (query) => query.state.data?.pendingItems ? 5_000 : false,
  });

  if (readiness.isPending) {
    return (
      <section className="lux-admin-panel lux-admin-user-content-index" aria-busy="true" aria-labelledby="lux-admin-user-content-index-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-content-index-title">内容分类索引</h2></div></div>
        <p className="lux-admin-muted" role="status">正在读取内容索引进度…</p>
      </section>
    );
  }

  if (readiness.error) {
    return (
      <section className="lux-admin-panel lux-admin-user-content-index" aria-labelledby="lux-admin-user-content-index-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-content-index-title">内容分类索引</h2></div></div>
        <p className="lux-error-copy" role="alert">读取索引进度失败：{readiness.error.message}</p>
        <button className="lux-button lux-button-secondary" type="button" onClick={() => void readiness.refetch()}>重试</button>
      </section>
    );
  }

  const { totalItems, indexedItems, pendingItems } = readiness.data;
  const hasNoItems = totalItems === 0;
  const isReady = !hasNoItems && pendingItems === 0;
  const statusLabel = hasNoItems ? "暂无媒体" : isReady ? "索引已就绪" : "正在回填";

  return (
    <section className="lux-admin-panel lux-admin-user-content-index" aria-labelledby="lux-admin-user-content-index-title">
      <div className="lux-admin-panel-heading">
        <div><span className="lux-admin-eyebrow">模块五</span><h2 id="lux-admin-user-content-index-title">内容分类索引</h2></div>
        <span className={`lux-user-badge ${isReady ? "is-ok" : hasNoItems ? "" : "is-warn"}`}>{statusLabel}</span>
      </div>
      {hasNoItems ? (
        <p className="lux-admin-muted" role="status">目前没有可统计的媒体条目。</p>
      ) : (
        <div className="lux-admin-user-content-index-progress" aria-live="polite">
          <div><span>已就绪</span><strong>{indexedItems.toLocaleString()} / {totalItems.toLocaleString()}</strong></div>
          <progress aria-label="已索引条目" max={totalItems} value={Math.min(indexedItems, totalItems)} />
          <p className="lux-admin-muted">待就绪 {pendingItems.toLocaleString()} 条。索引会在后台分批更新。</p>
        </div>
      )}
      <p className="lux-admin-muted lux-admin-user-content-index-note">此页可保存用户限制并展示索引回填状态；媒体访问拦截尚未接入，策略目前不会过滤媒体。</p>
    </section>
  );
}

function AdminUserSessionsSection({ user }: { user: AdminUser }) {
  return (
    <>
      <AdminUserDevicePanel user={user} />
      <AdminUserAccessRulesPanel userId={user.id} />
      <AdminUserWebSessionsPanel user={user} />
      <AdminUserEmbySessionsPanel user={user} />
      <AdminUserPlaybackSessionsPanel user={user} />
    </>
  );
}

function AdminUserDevicePanel({ user }: { user: AdminUser }) {
  const [page, setPage] = useState(1);
  const devicesQuery = useQuery({
    queryKey: ["admin", "user", user.id, "devices", page],
    queryFn: () => api.adminUserDevices(user.id, page, 20),
  });
  const devicePage = devicesQuery.data;
  const pageCount = devicePage
    ? Math.max(1, Math.ceil(devicePage.totalCount / devicePage.pageSize))
    : 1;

  if (devicesQuery.isPending) {
    return <AdminUserManagementState label="正在读取设备清单…" />;
  }
  if (devicesQuery.error) {
    return (
      <section className="lux-admin-panel lux-admin-user-sessions" aria-labelledby="lux-admin-user-devices-title">
        <div className="lux-admin-panel-heading">
          <div><span className="lux-admin-eyebrow">模块七</span><h2 id="lux-admin-user-devices-title">设备清单</h2></div>
        </div>
        <p role="alert">读取设备清单失败：{devicesQuery.error.message}</p>
        <button className="lux-button" type="button" onClick={() => void devicesQuery.refetch()}>重试</button>
      </section>
    );
  }
  if (!devicePage) {
    return <AdminUserManagementState label="正在读取设备清单…" />;
  }

  return (
    <section className="lux-admin-panel lux-admin-user-sessions lux-admin-user-devices" aria-labelledby="lux-admin-user-devices-title">
      <div className="lux-admin-panel-heading">
        <div>
          <span className="lux-admin-eyebrow">模块七</span>
          <h2 id="lux-admin-user-devices-title">设备清单</h2>
          <p className="lux-admin-user-session-summary">{devicePage.totalCount.toLocaleString()} 台已记录设备。设备标识由客户端提供，可能随客户端数据清除而变化。</p>
        </div>
      </div>
      {devicePage.devices.length === 0 ? (
        <p className="lux-admin-muted" role="status">尚无已记录的设备。</p>
      ) : (
        <div className="lux-admin-user-session-table-wrap">
          <table className="lux-admin-user-session-table lux-admin-user-device-table">
            <thead>
              <tr>
                <th scope="col">客户端 / 设备</th>
                <th scope="col">登录状态</th>
                <th scope="col">类型</th>
                <th scope="col">首次出现</th>
                <th scope="col">最近活动</th>
                <th scope="col">最近展示 IP</th>
              </tr>
            </thead>
            <tbody>
              {devicePage.devices.map((device) => (
                <tr key={device.deviceId}>
                  <th scope="row">
                    {device.clientName}
                    <span className="lux-admin-user-session-device-meta">
                      {device.deviceName || "未命名设备"} · ID：{device.deviceId}
                      {device.clientVersion ? ` · 版本 ${device.clientVersion}` : ""}
                    </span>
                  </th>
                  <td>
                    <span className={`lux-admin-device-login-status ${device.hasActiveLogin ? "is-active" : "is-inactive"}`}>
                      {device.hasActiveLogin ? "有有效登录" : "无有效登录"}
                    </span>
                  </td>
                  <td>{device.deviceType || "—"}</td>
                  <td><WebSessionDate value={device.firstSeenAt} /></td>
                  <td><WebSessionDate value={device.lastSeenAt} /></td>
                  <td>{device.lastRemoteIp || "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <nav className="lux-admin-user-session-pagination" aria-label="设备清单分页">
        <span>第 {devicePage.page} / {pageCount} 页</span>
        <div>
          <button
            className="lux-button"
            type="button"
            disabled={devicesQuery.isFetching || devicePage.page <= 1}
            onClick={() => setPage((currentPage) => Math.max(1, currentPage - 1))}
          >上一页</button>
          <button
            className="lux-button"
            type="button"
            disabled={devicesQuery.isFetching || devicePage.page >= pageCount}
            onClick={() => setPage((currentPage) => currentPage + 1)}
          >下一页</button>
        </div>
      </nav>
    </section>
  );
}

function AdminUserWebSessionsPanel({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [notice, setNotice] = useState("");
  const [actionError, setActionError] = useState("");
  const sessionsQueryKey = ["admin", "user", user.id, "web-sessions", page];
  const sessionsQuery = useQuery({
    queryKey: sessionsQueryKey,
    queryFn: () => api.adminUserWebSessions(user.id, page, 20),
  });
  const revokeSession = useMutation({
    mutationFn: (sessionId: string) => api.revokeAdminUserWebSession(user.id, sessionId),
    onSuccess: async (_result) => {
      setActionError("");
      setNotice("Web 登录已撤销。");
      if (page > 1 && sessionsQuery.data?.sessions.length === 1) {
        setPage((currentPage) => Math.max(1, currentPage - 1));
      }
      await queryClient.invalidateQueries({
        queryKey: ["admin", "user", user.id, "web-sessions"],
      });
    },
    onError: (cause) => {
      setNotice("");
      setActionError(cause.message || "撤销 Web 登录失败，请重试。");
    },
  });
  const sessionPage = sessionsQuery.data;
  const pageCount = sessionPage
    ? Math.max(1, Math.ceil(sessionPage.totalCount / sessionPage.pageSize))
    : 1;

  if (sessionsQuery.isPending) {
    return <AdminUserManagementState label="正在读取 Web 登录会话…" />;
  }
  if (sessionsQuery.error) {
    return (
      <section className="lux-admin-panel lux-admin-user-sessions lux-admin-user-web-sessions" aria-labelledby="lux-admin-user-sessions-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-sessions-title">Web 登录</h2></div></div>
        <p role="alert">读取登录会话失败：{sessionsQuery.error.message}</p>
        <button className="lux-button" type="button" onClick={() => void sessionsQuery.refetch()}>重试</button>
      </section>
    );
  }
  if (!sessionPage) {
    return <AdminUserManagementState label="暂时无法读取 Web 登录会话。" error />;
  }

  function requestRevoke(session: AdminUserWebSession) {
    if (session.isCurrent || revokeSession.isPending) return;
    if (!window.confirm("撤销此 Web 登录后，该浏览器下次访问时需要重新登录。确定撤销吗？")) return;
    setNotice("");
    setActionError("");
    revokeSession.mutate(session.id);
  }

  return (
    <section className="lux-admin-panel lux-admin-user-sessions lux-admin-user-web-sessions" aria-labelledby="lux-admin-user-sessions-title">
      <div className="lux-admin-panel-heading">
        <div>
          <h2 id="lux-admin-user-sessions-title">Web 登录</h2>
          <p className="lux-admin-user-session-summary">{sessionPage.totalCount} 个有效 Web 登录</p>
        </div>
      </div>
      {actionError ? <p role="alert">{actionError}</p> : null}
      {notice ? <p role="status">{notice}</p> : null}
      {sessionPage.sessions.length === 0 ? (
        <p className="lux-admin-muted">当前没有有效的 Web 登录会话。</p>
      ) : (
        <div className="lux-admin-user-session-table-wrap">
          <table className="lux-admin-user-session-table">
            <thead>
              <tr>
                <th scope="col">登录类型</th>
                <th scope="col">创建时间</th>
                <th scope="col">最近活动</th>
                <th scope="col">到期时间</th>
                <th scope="col">状态</th>
                <th scope="col">操作</th>
              </tr>
            </thead>
            <tbody>
              {sessionPage.sessions.map((session) => (
                <tr key={session.id}>
                  <th scope="row">浏览器 Web 登录</th>
                  <td><WebSessionDate value={session.createdAt} /></td>
                  <td>{session.lastSeenAt == null ? "最近活动未知" : <WebSessionDate value={session.lastSeenAt} />}</td>
                  <td><WebSessionDate value={session.expiresAt} /></td>
                  <td>
                    {session.isCurrent
                      ? <span className="lux-user-badge is-ok">当前会话</span>
                      : <span className="lux-admin-user-session-other">其他登录</span>}
                  </td>
                  <td>
                    {session.isCurrent
                      ? <span className="lux-admin-user-session-protected">当前登录不可撤销</span>
                      : (
                        <button
                          className="lux-button lux-admin-user-session-revoke"
                          type="button"
                          disabled={revokeSession.isPending}
                          onClick={() => requestRevoke(session)}
                        >
                          {revokeSession.isPending && revokeSession.variables === session.id ? "撤销中…" : "撤销登录"}
                        </button>
                      )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <nav className="lux-admin-user-session-pagination" aria-label="Web 登录分页">
        <span>第 {sessionPage.page} / {pageCount} 页</span>
        <div>
          <button
            className="lux-button"
            type="button"
            disabled={sessionsQuery.isFetching || sessionPage.page <= 1}
            onClick={() => setPage((currentPage) => Math.max(1, currentPage - 1))}
          >上一页</button>
          <button
            className="lux-button"
            type="button"
            disabled={sessionsQuery.isFetching || sessionPage.page >= pageCount}
            onClick={() => setPage((currentPage) => currentPage + 1)}
          >下一页</button>
        </div>
      </nav>
    </section>
  );
}

function AdminUserEmbySessionsPanel({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [notice, setNotice] = useState("");
  const [actionError, setActionError] = useState("");
  const sessionsQueryKey = ["admin", "user", user.id, "emby-sessions", page];
  const sessionsQuery = useQuery({
    queryKey: sessionsQueryKey,
    queryFn: () => api.adminUserEmbySessions(user.id, page, 20),
  });
  const revokeSession = useMutation({
    mutationFn: (sessionId: string) => api.revokeAdminUserEmbySession(user.id, sessionId),
    onSuccess: async () => {
      setActionError("");
      setNotice("Emby 登录已撤销。");
      if (page > 1 && sessionsQuery.data?.sessions.length === 1) {
        setPage((currentPage) => Math.max(1, currentPage - 1));
      }
      await queryClient.invalidateQueries({
        queryKey: ["admin", "user", user.id, "emby-sessions"],
      });
    },
    onError: (cause) => {
      setNotice("");
      setActionError(cause.message || "撤销 Emby 登录失败，请重试。");
    },
  });
  const revokeAllSessions = useMutation({
    mutationFn: () => api.revokeAllAdminUserEmbySessions(user.id),
    onSuccess: async ({ revokedCount }) => {
      setActionError("");
      setNotice("已撤销 " + revokedCount + " 个 Emby 登录凭证。");
      setPage(1);
      await queryClient.invalidateQueries({
        queryKey: ["admin", "user", user.id, "emby-sessions"],
      });
    },
    onError: (cause) => {
      setNotice("");
      setActionError(cause.message || "撤销全部 Emby 登录失败，请重试。");
    },
  });
  const sessionPage = sessionsQuery.data;
  const pageCount = sessionPage
    ? Math.max(1, Math.ceil(sessionPage.totalCount / sessionPage.pageSize))
    : 1;
  const totalSessionCount = sessionPage?.totalCount ?? 0;
  const isRevoking = revokeSession.isPending || revokeAllSessions.isPending;

  if (sessionsQuery.isPending) {
    return <AdminUserManagementState label="正在读取 Emby 登录凭证…" />;
  }
  if (sessionsQuery.error) {
    return (
      <section className="lux-admin-panel lux-admin-user-sessions" aria-labelledby="lux-admin-user-emby-sessions-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-emby-sessions-title">Emby 登录</h2></div></div>
        <p role="alert">读取 Emby 登录失败：{sessionsQuery.error.message}</p>
        <button className="lux-button" type="button" onClick={() => void sessionsQuery.refetch()}>重试</button>
      </section>
    );
  }
  if (!sessionPage) {
    return <AdminUserManagementState label="暂时无法读取 Emby 登录凭证。" error />;
  }

  function requestRevoke(session: AdminUserEmbySession) {
    if (isRevoking) return;
    if (!window.confirm("撤销此 Emby 登录凭证后，对应客户端需要重新登录。确定撤销吗？")) return;
    setNotice("");
    setActionError("");
    revokeSession.mutate(session.id);
  }

  function requestRevokeAll() {
    if (isRevoking || totalSessionCount === 0) return;
    if (!window.confirm(
      "即将撤销此用户全部 " + totalSessionCount + " 个有效 Emby 登录凭证，对应客户端需要重新登录。确定继续吗？",
    )) return;
    setNotice("");
    setActionError("");
    revokeAllSessions.mutate();
  }

  return (
    <section className="lux-admin-panel lux-admin-user-sessions" aria-labelledby="lux-admin-user-emby-sessions-title">
      <div className="lux-admin-panel-heading">
        <div>
          <h2 id="lux-admin-user-emby-sessions-title">Emby 登录</h2>
          <p className="lux-admin-user-session-summary">{sessionPage.totalCount} 个有效 Emby 登录凭证</p>
        </div>
        <button
          className="lux-button lux-admin-user-session-revoke-all"
          type="button"
          disabled={isRevoking || sessionsQuery.isFetching || sessionPage.totalCount === 0}
          onClick={requestRevokeAll}
        >
          {revokeAllSessions.isPending ? "正在撤销…" : "撤销全部 Emby 登录"}
        </button>
      </div>
      {actionError ? <p role="alert">{actionError}</p> : null}
      {notice ? <p role="status">{notice}</p> : null}
      {sessionPage.sessions.length === 0 ? (
        <p className="lux-admin-muted">当前没有有效的 Emby 登录凭证。</p>
      ) : (
        <div className="lux-admin-user-session-table-wrap">
          <table className="lux-admin-user-session-table lux-admin-user-emby-session-table">
            <thead>
              <tr>
                <th scope="col">设备</th>
                <th scope="col">客户端</th>
                <th scope="col">首次登录</th>
                <th scope="col">最近活动</th>
                <th scope="col">操作</th>
              </tr>
            </thead>
            <tbody>
              {sessionPage.sessions.map((session) => (
                <tr key={session.id}>
                  <th scope="row">
                    {session.deviceName || "未知设备"}
                    <span className="lux-admin-user-session-device-meta">
                      设备 ID：{session.deviceId || "未知"}
                      {session.deviceType ? " · " + session.deviceType : ""}
                    </span>
                  </th>
                  <td>{[session.clientName, session.clientVersion].filter(Boolean).join(" ") || "未知客户端"}</td>
                  <td><WebSessionDate value={session.createdAt} /></td>
                  <td>{session.lastSeenAt == null ? "最近活动未知" : <WebSessionDate value={session.lastSeenAt} />}</td>
                  <td>
                    <button
                      className="lux-button lux-admin-user-session-revoke"
                      type="button"
                      disabled={isRevoking}
                      onClick={() => requestRevoke(session)}
                    >
                      {revokeSession.isPending && revokeSession.variables === session.id ? "撤销中…" : "撤销登录"}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <nav className="lux-admin-user-session-pagination" aria-label="Emby 登录分页">
        <span>第 {sessionPage.page} / {pageCount} 页</span>
        <div>
          <button
            className="lux-button"
            type="button"
            disabled={sessionsQuery.isFetching || sessionPage.page <= 1}
            onClick={() => setPage((currentPage) => Math.max(1, currentPage - 1))}
          >上一页</button>
          <button
            className="lux-button"
            type="button"
            disabled={sessionsQuery.isFetching || sessionPage.page >= pageCount}
            onClick={() => setPage((currentPage) => currentPage + 1)}
          >下一页</button>
        </div>
      </nav>
    </section>
  );
}

function WebSessionDate({ value }: { value: number }) {
  const date = new Date(value * 1000);
  if (!Number.isFinite(date.getTime())) return <span>—</span>;
  return (
    <time dateTime={date.toISOString()}>
      {new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date)}
    </time>
  );
}

function AdminUserPlaybackSessionsPanel({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [notice, setNotice] = useState("");
  const [actionError, setActionError] = useState("");
  const sessionsQuery = useQuery({
    queryKey: ["admin", "user", user.id, "playback-sessions", page],
    queryFn: () => api.adminUserPlaybackSessions(user.id, page, 20),
  });
  const stopSession = useMutation({
    mutationFn: (playbackId: string) => api.stopAdminUserPlaybackSession(user.id, playbackId),
    onSuccess: async () => {
      setActionError("");
      setNotice("播放已停止。");
      if (page > 1 && sessionsQuery.data?.sessions.length === 1) {
        setPage((currentPage) => Math.max(1, currentPage - 1));
      }
      await queryClient.invalidateQueries({
        queryKey: ["admin", "user", user.id, "playback-sessions"],
      });
    },
    onError: (cause) => {
      setNotice("");
      setActionError(cause.message || "停止播放失败，请重试。");
    },
  });
  const sessionPage = sessionsQuery.data;
  const pageCount = sessionPage
    ? Math.max(1, Math.ceil(sessionPage.totalCount / sessionPage.pageSize))
    : 1;

  if (sessionsQuery.isPending) {
    return <AdminUserManagementState label="正在读取当前播放…" />;
  }
  if (sessionsQuery.error) {
    return (
      <section className="lux-admin-panel lux-admin-user-playback-sessions" aria-labelledby="lux-admin-user-playback-sessions-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-playback-sessions-title">当前播放</h2></div></div>
        <p role="alert">读取当前播放失败：{sessionsQuery.error.message}</p>
        <button className="lux-button" type="button" onClick={() => void sessionsQuery.refetch()}>重试</button>
      </section>
    );
  }
  if (!sessionPage) {
    return <AdminUserManagementState label="暂时无法读取当前播放。" error />;
  }

  function requestStop(session: AdminUserPlaybackSession) {
    if (!session.canStop || stopSession.isPending) return;
    if (!window.confirm("将停止此用户通过 Lux 服务端 HLS 进行的播放。用户登录仍然有效。确定停止吗？")) return;
    setNotice("");
    setActionError("");
    stopSession.mutate(session.id);
  }

  return (
    <section className="lux-admin-panel lux-admin-user-playback-sessions" aria-labelledby="lux-admin-user-playback-sessions-title">
      <div className="lux-admin-panel-heading">
        <div>
          <h2 id="lux-admin-user-playback-sessions-title">当前播放</h2>
          <p className="lux-admin-user-session-summary">{sessionPage.totalCount} 个活跃播放会话</p>
        </div>
      </div>
      <p className="lux-admin-muted lux-admin-user-playback-boundary">
        仅 Lux 服务端 HLS 会话可由此处停止。直放与 STRM 外部播放无法保证立即中断；停止播放不会撤销用户登录。
      </p>
      {actionError ? <p className="lux-error-copy" role="alert">停止播放失败：{actionError}</p> : null}
      {notice ? <p role="status">{notice}</p> : null}
      {sessionPage.sessions.length === 0 ? (
        <p className="lux-admin-muted" role="status">当前没有活跃的播放会话。</p>
      ) : (
        <div className="lux-admin-user-session-table-wrap">
          <table className="lux-admin-user-session-table lux-admin-user-playback-table">
            <thead>
              <tr>
                <th scope="col">媒体</th>
                <th scope="col">进度</th>
                <th scope="col">播放方式</th>
                <th scope="col">客户端与设备</th>
                <th scope="col">来源 IP</th>
                <th scope="col">状态</th>
                <th scope="col">操作</th>
              </tr>
            </thead>
            <tbody>
              {sessionPage.sessions.map((session) => (
                <tr key={session.id}>
                  <th scope="row">
                    {session.title}
                    {session.seriesTitle ? (
                      <span className="lux-admin-user-session-device-meta">
                        {session.seriesTitle}
                        {session.parentIndexNumber != null ? ` · 第 ${session.parentIndexNumber} 季` : ""}
                        {session.indexNumber != null ? ` 第 ${session.indexNumber} 集` : ""}
                      </span>
                    ) : null}
                  </th>
                  <td>
                    <div className="lux-admin-user-playback-progress">
                      {session.durationTicks != null && session.durationTicks > 0 ? (
                        <progress
                          aria-label={`${session.title} 播放进度`}
                          max={session.durationTicks}
                          value={Math.min(Math.max(session.positionTicks, 0), session.durationTicks)}
                        />
                      ) : null}
                      <span>{playbackProgressLabel(session)}</span>
                    </div>
                  </td>
                  <td>{playbackMethodLabel(session)}</td>
                  <td>
                    {[session.client, session.clientVersion].filter(Boolean).join(" ") || "未知客户端"}
                    <span className="lux-admin-user-session-device-meta">
                      {session.deviceName || "未知设备"}{session.deviceType ? ` · ${session.deviceType}` : ""}
                    </span>
                  </td>
                  <td>
                    {session.remoteIp || "未知"}
                    {session.remoteIpLocation?.location ? (
                      <span className="lux-admin-user-session-device-meta">{session.remoteIpLocation.location}</span>
                    ) : null}
                  </td>
                  <td>{session.isPaused || session.state === "PAUSED" ? "已暂停" : "正在播放"}</td>
                  <td>
                    {session.canStop ? (
                      <button
                        className="lux-button lux-admin-user-session-revoke lux-admin-user-playback-stop"
                        type="button"
                        disabled={stopSession.isPending}
                        onClick={() => requestStop(session)}
                      >
                        {stopSession.isPending && stopSession.variables === session.id ? "停止中…" : "停止播放"}
                      </button>
                    ) : <span className="lux-admin-user-session-protected">无法远程停止</span>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {sessionPage.totalCount > 0 ? (
        <nav className="lux-admin-user-session-pagination" aria-label="当前播放分页">
          <span>第 {sessionPage.page} / {pageCount} 页</span>
          <div>
            <button
              className="lux-button"
              type="button"
              disabled={sessionsQuery.isFetching || sessionPage.page <= 1}
              onClick={() => setPage((currentPage) => Math.max(1, currentPage - 1))}
            >上一页</button>
            <button
              className="lux-button"
              type="button"
              disabled={sessionsQuery.isFetching || sessionPage.page >= pageCount}
              onClick={() => setPage((currentPage) => currentPage + 1)}
            >下一页</button>
          </div>
        </nav>
      ) : null}
    </section>
  );
}

function playbackProgressLabel(session: AdminUserPlaybackSession) {
  if (session.durationTicks == null || session.durationTicks <= 0) return "进度未知";
  const position = formatPlaybackTicks(session.positionTicks);
  const duration = formatPlaybackTicks(session.durationTicks);
  const percent = Math.round(Math.min(Math.max(session.positionTicks, 0), session.durationTicks) / session.durationTicks * 100);
  return `${percent}% · ${position} / ${duration}`;
}

function formatPlaybackTicks(ticks: number) {
  const totalSeconds = Math.max(0, Math.floor(ticks / 10_000_000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`
    : `${minutes}:${String(seconds).padStart(2, "0")}`;
}

function playbackMethodLabel(session: AdminUserPlaybackSession) {
  if (session.playbackPlan === "SERVER_HLS") return "Lux 服务端 HLS";
  if (session.playMethod === "DirectStream") return "直通播放";
  if (session.playMethod === "Transcode") return "服务端转码";
  return "直放 / 外部播放";
}

const activityEventLabels: Record<string, string> = {
  AUTH_LOGIN: "成功登录",
  AUTH_LOGIN_FAILED: "登录失败",
  PLAYBACK_STARTED: "开始播放",
  PLAYBACK_PAUSED: "暂停播放",
  PLAYBACK_STOPPED: "停止播放",
  USER_ACCESS_BLACKLIST_HIT: "访问黑名单命中",
  USER_ACCESS_RULES_DENIED: "访问规则拒绝",
};

const activityMetadataFields = [
  ["deviceName", "设备"],
  ["deviceType", "设备类型"],
  ["client", "客户端"],
  ["clientVersion", "客户端版本"],
  ["state", "播放状态"],
  ["remoteIp", "IP"],
  ["reason", "命中原因"],
] as const;

function AdminUserActivityPanel({ user }: { user: AdminUser }) {
  const [page, setPage] = useState(1);
  const [eventTypeDraft, setEventTypeDraft] = useState("");
  const [fromDraft, setFromDraft] = useState("");
  const [toDraft, setToDraft] = useState("");
  const [filters, setFilters] = useState<{ eventType?: string; from?: number; to?: number }>({});
  const [filterError, setFilterError] = useState("");
  const activityQuery = useQuery({
    queryKey: ["admin", "user", user.id, "activity", page, filters],
    queryFn: () => api.adminUserActivity(user.id, { page, pageSize: 20, ...filters }),
  });
  const activityPage = activityQuery.data;
  const pageCount = activityPage
    ? Math.max(1, Math.ceil(activityPage.totalCount / activityPage.pageSize))
    : 1;

  function applyFilters(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const from = dateTimeLocalToUnix(fromDraft);
    const to = dateTimeLocalToUnix(toDraft, true);
    if ((fromDraft && from == null) || (toDraft && to == null)) {
      setFilterError("请输入有效的起止时间。");
      return;
    }
    if (from != null && to != null && from > to) {
      setFilterError("开始时间不能晚于结束时间。");
      return;
    }
    setFilterError("");
    setPage(1);
    setFilters({
      ...(eventTypeDraft ? { eventType: eventTypeDraft } : {}),
      ...(from != null ? { from } : {}),
      ...(to != null ? { to } : {}),
    });
  }

  function clearFilters() {
    setEventTypeDraft("");
    setFromDraft("");
    setToDraft("");
    setFilterError("");
    setPage(1);
    setFilters({});
  }

  return (
    <section className="lux-admin-panel lux-admin-user-activity" aria-labelledby="lux-admin-user-activity-title">
      <div className="lux-admin-panel-heading">
        <div>
          <span className="lux-admin-eyebrow">模块七</span>
          <h2 id="lux-admin-user-activity-title">活动记录</h2>
          {activityPage ? <p className="lux-admin-user-activity-summary">共 {activityPage.totalCount} 条</p> : null}
        </div>
      </div>
      <form className="lux-admin-user-activity-filters" onSubmit={applyFilters}>
        <label>
          事件类型
          <select aria-label="事件类型" value={eventTypeDraft} onChange={(event) => setEventTypeDraft(event.currentTarget.value)}>
            <option value="">全部事件</option>
            <option value="AUTH_LOGIN">成功登录</option>
            <option value="AUTH_LOGIN_FAILED">登录失败</option>
            <option value="PLAYBACK_STARTED">开始播放</option>
            <option value="PLAYBACK_PAUSED">暂停播放</option>
            <option value="PLAYBACK_STOPPED">停止播放</option>
          </select>
        </label>
        <label>
          开始时间
          <input aria-label="开始时间" type="datetime-local" value={fromDraft} onChange={(event) => setFromDraft(event.currentTarget.value)} />
        </label>
        <label>
          结束时间
          <input aria-label="结束时间" type="datetime-local" value={toDraft} onChange={(event) => setToDraft(event.currentTarget.value)} />
        </label>
        <div className="lux-admin-user-activity-filter-actions">
          <button className="lux-button lux-button-primary" type="submit">应用筛选</button>
          <button className="lux-button" type="button" onClick={clearFilters}>清除</button>
        </div>
      </form>
      {filterError ? <p className="lux-error-copy" role="alert">{filterError}</p> : null}
      {activityQuery.isPending ? (
        <p className="lux-admin-muted" role="status" aria-busy="true">正在读取活动记录…</p>
      ) : activityQuery.error ? (
        <div className="lux-admin-user-activity-state">
          <p className="lux-error-copy" role="alert">读取活动记录失败：{activityQuery.error.message}</p>
          <button className="lux-button" type="button" onClick={() => void activityQuery.refetch()}>重试</button>
        </div>
      ) : !activityPage ? (
        <p className="lux-admin-muted" role="status">暂时无法读取活动记录。</p>
      ) : activityPage.events.length === 0 ? (
        <p className="lux-admin-muted" role="status">没有符合条件的活动记录。</p>
      ) : (
        <div className="lux-admin-user-activity-table-wrap">
          <table className="lux-admin-user-activity-table">
            <thead>
              <tr>
                <th scope="col">事件</th>
                <th scope="col">发生时间</th>
                <th scope="col">目标</th>
                <th scope="col">设备与来源</th>
              </tr>
            </thead>
            <tbody>
              {activityPage.events.map((activity) => <AdminUserActivityRow key={activity.id} activity={activity} />)}
            </tbody>
          </table>
        </div>
      )}
      {activityPage ? (
        <nav className="lux-admin-user-session-pagination" aria-label="活动记录分页">
          <span>第 {activityPage.page} / {pageCount} 页</span>
          <span>共 {activityPage.totalCount} 条</span>
          <div>
            <button
              className="lux-button"
              type="button"
              disabled={activityQuery.isFetching || activityPage.page <= 1}
              onClick={() => setPage((currentPage) => Math.max(1, currentPage - 1))}
            >上一页</button>
            <button
              className="lux-button"
              type="button"
              disabled={activityQuery.isFetching || activityPage.page >= pageCount}
              onClick={() => setPage((currentPage) => currentPage + 1)}
            >下一页</button>
          </div>
        </nav>
      ) : null}
    </section>
  );
}

function AdminUserActivityRow({ activity }: { activity: AdminUserActivityEvent }) {
  const metadata = activityMetadataFields.flatMap(([key, label]) => {
    const value = activity.metadata[key];
    if (typeof value !== "string" && typeof value !== "number") return [];
    const safeValue = String(value).trim();
    return safeValue ? [{ label, value: safeValue.slice(0, 200) }] : [];
  });
  return (
    <tr>
      <th scope="row">
        {activityEventLabels[activity.eventType] ?? activity.eventType}
        <span className="lux-admin-user-activity-event-type">{activity.eventType}</span>
      </th>
      <td><WebSessionDate value={activity.occurredAt} /></td>
      <td>
        {activity.targetId
          ? <span>{activity.targetType ? activity.targetType + " · " : ""}{activity.targetId}</span>
          : <span className="lux-admin-muted">—</span>}
      </td>
      <td>
        {metadata.length > 0 ? (
          <ul className="lux-admin-user-activity-metadata">
            {metadata.map((item) => <li key={item.label}><span>{item.label}</span><span>{item.value}</span></li>)}
          </ul>
        ) : <span className="lux-admin-muted">—</span>}
      </td>
    </tr>
  );
}

function dateTimeLocalToUnix(value: string, inclusiveMinute = false): number | undefined {
  if (!value) return undefined;
  const milliseconds = new Date(value).getTime();
  return Number.isFinite(milliseconds) ? Math.floor(milliseconds / 1000) + (inclusiveMinute ? 59 : 0) : undefined;
}

function AdminUserPermissionsSection({ user }: { user: AdminUser }) {
  if (user.deletedAt != null) {
    return (
      <section className="lux-admin-panel lux-admin-user-permissions" aria-labelledby="lux-admin-user-permissions-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-permissions-title">权限</h2></div></div>
        <p className="lux-admin-muted">回收站中的账号权限为只读，恢复后可修改。</p>
      </section>
    );
  }
  return (
    <>
      <AdminUserPermissionPresetEditor user={user} />
      <AdminUserLibraryAccessEditor user={user} />
    </>
  );
}

function AdminUserPermissionPresetEditor({ user }: { user: AdminUser }) {
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

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!selectedPreset || apply.isPending) return;
    const displayName = user.displayName || user.usernameNormalized;
    const accepted = window.confirm(
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
          <select
            id={`lux-admin-user-preset-${user.id}`}
            aria-label="选择权限预设"
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
          </select>
        </label>
        <p className="lux-admin-muted">应用会覆盖该用户当前的角色、媒体库、下载、播放和转码权限；观看记录、偏好及登录凭证不会改变。</p>
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-primary" type="submit" disabled={!selectedPreset || apply.isPending || Boolean(presetsQuery.error)}>
            {apply.isPending ? "应用中…" : "应用所选预设"}
          </button>
          {error ? <span role="alert">应用失败：{error}</span> : null}
          {notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

function AdminUserLibraryAccessEditor({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const queryKey = ["admin", "user-access", user.id];
  const accessQuery = useQuery({
    queryKey,
    queryFn: () => api.adminUserLibraryAccess(user.id),
  });
  const librariesQuery = useQuery({
    queryKey: queryKeys.adminLibraries,
    queryFn: () => api.adminLibraries(),
  });
  const [modeDraft, setModeDraft] = useState<AdminUserLibraryAccessMode | null>(null);
  const [libraryDraft, setLibraryDraft] = useState<string[] | null>(null);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const availableLibraries = librariesQuery.data?.libraries ?? [];
  const enabledLibraries = availableLibraries.filter((library) => library.isEnabled);
  const enabledLibraryIds = new Set(enabledLibraries.map((library) => library.id));
  const storedMode = accessQuery.data?.libraryAccessMode;
  const draftMode = modeDraft ?? storedMode ?? "ALL";
  const storedLibraryIds = accessQuery.data?.libraryIds ?? [];
  const draftLibraryIds = libraryDraft ?? storedLibraryIds;
  const enabledDraftIds = draftLibraryIds.filter((id) => enabledLibraryIds.has(id));
  const disabledDraftNames = draftLibraryIds
    .filter((id) => !enabledLibraryIds.has(id))
    .map((id) => availableLibraries.find((library) => library.id === id)?.name ?? id);
  const saveMode: AdminUserLibraryAccessMode = draftMode === "SELECTED" && enabledDraftIds.length === 0
    ? "NONE"
    : draftMode;
  const saveLibraryIds = saveMode === "SELECTED" ? enabledDraftIds : [];
  const savedLibraryIds = storedMode === "SELECTED"
    ? storedLibraryIds.filter((id) => enabledLibraryIds.has(id))
    : [];
  const hasChanges = storedMode != null && (
    saveMode !== storedMode
    || saveLibraryIds.length !== savedLibraryIds.length
    || saveLibraryIds.some((id, index) => id !== savedLibraryIds[index])
  );
  const update = useMutation({
    mutationFn: () => api.updateAdminUserLibraryAccess(user.id, {
      libraryAccessMode: saveMode,
      libraryIds: saveLibraryIds,
    }),
    onSuccess: (result) => {
      setModeDraft(result.libraryAccessMode);
      setLibraryDraft(result.libraryIds);
      setError("");
      setNotice("媒体库访问权限已保存。");
      queryClient.setQueryData(queryKey, result);
    },
    onError: (cause) => {
      setNotice("");
      setError(cause.message || "媒体库权限保存失败，请重试。");
    },
  });
  useEffect(() => {
    if (!accessQuery.data) return;
    setModeDraft(accessQuery.data.libraryAccessMode);
    setLibraryDraft(accessQuery.data.libraryIds);
  }, [accessQuery.data, user.id]);

  if (accessQuery.isPending || librariesQuery.isPending) {
    return <AdminUserManagementState label="正在读取媒体库权限…" />;
  }
  if (accessQuery.error) {
    return <AdminUserManagementState label={`读取媒体库权限失败：${accessQuery.error.message}`} error />;
  }
  if (librariesQuery.error) {
    return <AdminUserManagementState label={`读取媒体库列表失败：${librariesQuery.error.message}`} error />;
  }
  if (!accessQuery.data) {
    return <AdminUserManagementState label="暂时无法读取媒体库权限。" error />;
  }

  function toggleLibrary(libraryId: string, checked: boolean) {
    setLibraryDraft((current) => {
      const currentIds = current ?? storedLibraryIds;
      const next = new Set(currentIds);
      if (checked) next.add(libraryId);
      else next.delete(libraryId);
      const enabledIds = enabledLibraries.map((library) => library.id).filter((id) => next.has(id));
      return [...enabledIds, ...currentIds.filter((id) => !enabledLibraryIds.has(id))];
    });
    setNotice("");
    setError("");
  }

  return (
    <section className="lux-admin-panel lux-admin-user-permissions" aria-labelledby="lux-admin-user-permissions-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-permissions-title">权限</h2></div></div>
      <form
        className="lux-admin-form lux-admin-user-access-form"
        onSubmit={(event) => {
          event.preventDefault();
          setNotice("");
          setError("");
          update.mutate();
        }}
      >
        <fieldset className="lux-admin-user-access-modes">
          <legend>媒体库访问范围</legend>
          <label>
            <input
              type="radio"
              name={`library-access-${user.id}`}
              value="ALL"
              checked={draftMode === "ALL"}
              disabled={update.isPending}
              onChange={() => { setModeDraft("ALL"); setNotice(""); setError(""); }}
            />
            <span><strong>全部媒体库</strong><small>允许访问所有已启用媒体库，包括之后新增的媒体库。</small></span>
          </label>
          <label>
            <input
              type="radio"
              name={`library-access-${user.id}`}
              value="SELECTED"
              checked={draftMode === "SELECTED"}
              disabled={update.isPending}
              onChange={() => { setModeDraft("SELECTED"); setNotice(""); setError(""); }}
            />
            <span><strong>指定媒体库</strong><small>仅允许访问下方勾选的媒体库。</small></span>
          </label>
          <label>
            <input
              type="radio"
              name={`library-access-${user.id}`}
              value="NONE"
              checked={draftMode === "NONE"}
              disabled={update.isPending}
              onChange={() => { setModeDraft("NONE"); setNotice(""); setError(""); }}
            />
            <span><strong>无媒体库权限</strong><small>用户不能浏览、搜索或播放任何媒体库内容。</small></span>
          </label>
        </fieldset>
        {draftMode === "ALL" ? <p className="lux-admin-muted">可访问所有已启用媒体库，包括之后新增的媒体库。</p> : null}
        {draftMode === "NONE" ? <p className="lux-admin-muted">不能访问任何媒体库。</p> : null}
        {draftMode === "SELECTED" ? (
          <>
            {disabledDraftNames.length ? (
              <p className="lux-admin-muted">已停用媒体库当前不可访问；保存指定范围时会移除：{disabledDraftNames.join("、")}。</p>
            ) : null}
            <p className="lux-admin-muted">
              {enabledLibraries.length
                ? enabledDraftIds.length
                  ? `已选择 ${enabledDraftIds.length} 个媒体库。`
                  : "请至少选择一个媒体库；空选择保存后会设为无媒体库权限。"
                : "当前没有已启用的媒体库；保存空选择后会设为无媒体库权限。"}
            </p>
            {enabledLibraries.length ? (
              <div className="lux-admin-library-access">
                {enabledLibraries.map((library) => (
                  <label key={library.id}>
                    <input
                      type="checkbox"
                      checked={enabledDraftIds.includes(library.id)}
                      disabled={update.isPending}
                      onChange={(event) => toggleLibrary(library.id, event.currentTarget.checked)}
                    />
                    <span>{library.name}</span>
                  </label>
                ))}
              </div>
            ) : null}
          </>
        ) : null}
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-primary" type="submit" disabled={!hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存媒体库权限"}
          </button>
          {error ? <span role="alert">保存失败：{error}</span> : null}
          {notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

function AdminUserPlaybackPolicySection({ user }: { user: AdminUser }) {
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
          <label className="lux-admin-user-playback-toggle">
            <input type="checkbox" aria-label="允许播放" checked={currentDraft.canPlay} onChange={(event) => setDraft({ ...currentDraft, canPlay: event.currentTarget.checked })} />
            <span>允许播放</span>
          </label>
          <label className="lux-admin-user-playback-toggle">
            <input type="checkbox" aria-label="允许封装转换" checked={currentDraft.allowRemux} onChange={(event) => setDraft({ ...currentDraft, allowRemux: event.currentTarget.checked })} />
            <span>允许封装转换</span>
          </label>
          <label className="lux-admin-user-playback-toggle">
            <input type="checkbox" aria-label="允许音频转码" checked={currentDraft.allowAudioTranscode} onChange={(event) => setDraft({ ...currentDraft, allowAudioTranscode: event.currentTarget.checked })} />
            <span>允许音频转码</span>
          </label>
          <label className="lux-admin-user-playback-toggle">
            <input type="checkbox" aria-label="允许视频转码" checked={currentDraft.allowVideoTranscode} onChange={(event) => setDraft({ ...currentDraft, allowVideoTranscode: event.currentTarget.checked })} />
            <span>允许视频转码</span>
          </label>
        </fieldset>
        <label className="lux-admin-user-playback-limit">
          同时播放上限
          <input
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
        <p className="lux-admin-muted">播放准入策略将在下一步生效；目前保存的设置尚未影响播放请求。</p>
        {limitInput.trim() === "" || !validLimit ? <p className="lux-admin-user-playback-validation" role="alert">请输入非负整数。</p> : null}
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-primary" type="submit" disabled={!hasChanges || !validLimit || save.isPending}>
            {save.isPending ? "保存中…" : "保存播放策略"}
          </button>
          {error ? <span role="alert">{error}</span> : null}
          {notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

function AdminUserProfileEditor({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const [displayName, setDisplayName] = useState(user.displayName ?? "");
  const [email, setEmail] = useState(user.email ?? "");
  const [notice, setNotice] = useState("");
  const [avatarNotice, setAvatarNotice] = useState("");
  const [avatarVersion, setAvatarVersion] = useState(() => String(Date.now()));
  const [avatarFailed, setAvatarFailed] = useState(false);
  const update = useMutation({
    mutationFn: () => api.updateAdminUserProfile(user.id, { displayName, email }),
    onSuccess: ({ user: updated }) => {
      setDisplayName(updated.displayName ?? "");
      setEmail(updated.email ?? "");
      setNotice("账户资料已保存。");
      queryClient.setQueryData(["admin", "user", user.id], { user: updated });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
    },
    onError: (error) => setNotice(error.message || "资料保存失败，请重试。"),
  });
  const updateAvatar = useMutation({
    mutationFn: (file: File) => api.uploadAdminUserAvatar(user.id, file),
    onSuccess: () => {
      setAvatarVersion(String(Date.now()));
      setAvatarFailed(false);
      setAvatarNotice("头像已更新。");
    },
    onError: (error) => setAvatarNotice(error.message || "头像更新失败，请重试。"),
  });
  useEffect(() => {
    setDisplayName(user.displayName ?? "");
    setEmail(user.email ?? "");
    setNotice("");
  }, [user.displayName, user.email, user.id]);

  const emailStatus = user.email
    ? user.emailVerifiedAt ? "已验证" : "未验证"
    : "未填写";
  const hasChanges = displayName !== (user.displayName ?? "") || email !== (user.email ?? "");

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setNotice("");
    update.mutate();
  }

  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-profile-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-profile-title">资料与状态</h2></div>
        <span className={user.deletedAt != null || user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>
          {user.deletedAt != null ? "回收站" : user.isDisabled ? "已禁用" : "正常"}
        </span>
      </div>
      <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit}>
        <div className="lux-admin-user-avatar-row">
          <span className="lux-admin-user-avatar is-profile">
            {avatarFailed ? <UserRound size={27} /> : <img src={api.adminUserAvatarUrl(user.id, avatarVersion)} alt="" onError={() => setAvatarFailed(true)} />}
          </span>
          <div className="lux-admin-user-avatar-actions">
            <strong>头像</strong>
            <label className="lux-button lux-button-secondary lux-admin-user-avatar-upload">
              <span>{updateAvatar.isPending ? "上传中…" : "更换头像"}</span>
              <input
                type="file"
                accept="image/jpeg,image/png,image/webp"
                aria-label="更换用户头像"
                disabled={user.deletedAt != null || updateAvatar.isPending}
                onChange={(event) => {
                  const file = event.currentTarget.files?.[0];
                  event.currentTarget.value = "";
                  if (file) {
                    setAvatarNotice("");
                    updateAvatar.mutate(file);
                  }
                }}
              />
            </label>
            {avatarNotice ? <span role={updateAvatar.isError ? "alert" : "status"}>{avatarNotice}</span> : null}
          </div>
        </div>
        <label>
          显示名称
          <input value={displayName} onChange={(event) => setDisplayName(event.target.value)} maxLength={128} disabled={user.deletedAt != null} />
        </label>
        <label>
          邮箱
          <input type="email" value={email} onChange={(event) => setEmail(event.target.value)} autoComplete="email" disabled={user.deletedAt != null} />
        </label>
        <p className="lux-admin-muted">{user.deletedAt != null ? "回收站中的账号资料为只读。" : `邮箱状态：${emailStatus}。邮箱变更会清除验证状态；验证流程将在邮件功能中接入。`}</p>
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-primary" type="submit" disabled={user.deletedAt != null || !hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存资料"}
          </button>
          {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

function AdminUserLifecycleActions({ user }: { user: AdminUser }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [error, setError] = useState("");
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const graceDays = settings.data?.userDeletionGraceDays;
  const refresh = (updated: AdminUser) => {
    queryClient.setQueryData(["admin", "user", user.id], { user: updated });
    void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
  };
  const disable = useMutation({ mutationFn: () => api.disableAdminUser(user.id), onSuccess: ({ user: updated }) => refresh(updated), onError: (cause) => setError(cause.message) });
  const enable = useMutation({ mutationFn: () => api.updateAdminUser(user.id, { isDisabled: false }), onSuccess: ({ user: updated }) => refresh(updated), onError: (cause) => setError(cause.message) });
  const remove = useMutation({ mutationFn: () => api.deleteAdminUser(user.id), onSuccess: ({ user: updated }) => { refresh(updated); navigate("/admin/users?view=deleted"); }, onError: (cause) => setError(cause.message) });
  const restore = useMutation({ mutationFn: () => api.restoreAdminUser(user.id), onSuccess: ({ user: updated }) => { refresh(updated); navigate("/admin/users"); }, onError: (cause) => setError(cause.message) });
  const purge = useMutation({ mutationFn: () => api.purgeAdminUser(user.id), onSuccess: () => { void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }); navigate("/admin/users?view=deleted"); }, onError: (cause) => setError(cause.message) });
  const pending = disable.isPending || enable.isPending || remove.isPending || restore.isPending || purge.isPending;
  const displayName = user.displayName || user.usernameNormalized;

  function confirmDisable() {
    if (pending || !window.confirm(`禁用“${displayName}”后会撤销登录凭证并停止播放，账号资料仍会保留。确定继续吗？`)) return;
    setError("");
    disable.mutate();
  }

  function confirmEnable() {
    if (pending || !window.confirm(`启用“${displayName}”后，用户可以重新登录。确定继续吗？`)) return;
    setError("");
    enable.mutate();
  }

  function confirmDelete() {
    const retention = graceDays === undefined ? "按服务器当前配置的期限保留" : `保留 ${graceDays} 天`;
    if (pending || !window.confirm(`将“${displayName}”移入回收站会撤销凭证并停止播放，账号会${retention}，可在期限内恢复。确定继续吗？`)) return;
    setError("");
    remove.mutate();
  }

  function confirmPurge() {
    if (pending || !window.confirm(`彻底删除“${displayName}”将永久删除账号、头像、权限设置、偏好、播放记录和设备关联，媒体文件不受影响，且无法恢复。确定继续吗？`)) return;
    setError("");
    purge.mutate();
  }

  return (
    <section className="lux-admin-panel lux-admin-user-lifecycle-panel" aria-labelledby="lux-admin-user-lifecycle-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-lifecycle-title">账号状态</h2></div><span className={user.deletedAt != null || user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>{user.deletedAt != null ? "回收站" : user.isDisabled ? "已禁用" : "正常"}</span></div>
      {user.deletedAt != null ? <p className="lux-admin-muted">删除于 {user.deletedAt ? new Date(user.deletedAt * 1000).toLocaleString() : "未知"} · 计划清理 {user.deletionPurgeAt ? new Date(user.deletionPurgeAt * 1000).toLocaleString() : "未知"}</p> : null}
      <div className="lux-admin-user-profile-actions">
        {user.deletedAt != null ? <>
          <button className="lux-button lux-button-secondary" type="button" onClick={() => restore.mutate()} disabled={pending}>{restore.isPending ? "恢复中…" : "恢复账号"}</button>
          <button className="lux-button lux-button-secondary lux-admin-purge-user" type="button" onClick={confirmPurge} disabled={pending}>{purge.isPending ? "删除中…" : "彻底删除"}</button>
        </> : <>
          {user.isDisabled
            ? <button className="lux-button lux-button-secondary" type="button" onClick={confirmEnable} disabled={pending}>{enable.isPending ? "启用中…" : "启用账号"}</button>
            : <button className="lux-button lux-button-secondary" type="button" onClick={confirmDisable} disabled={pending}>{disable.isPending ? "禁用中…" : "禁用账号"}</button>}
          <button className="lux-button lux-button-secondary lux-admin-purge-user" type="button" onClick={confirmDelete} disabled={pending}>{remove.isPending ? "移入回收站…" : "移入回收站"}</button>
        </>}
        {error ? <span role="alert">操作失败：{error}</span> : null}
      </div>
    </section>
  );
}

export function AdminUserSettingsPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const sectionId = searchParams.get("section") ?? "registration";
  const section = settingsSections.find((item) => item.id === sectionId) ?? settingsSections[0];

  return (
    <div className="lux-admin-page lux-admin-user-management-page">
      <Link className="lux-admin-user-back" to="/admin/users"><ArrowLeft size={15} />返回用户列表</Link>
      <header className="lux-admin-user-page-heading">
        <div><span className="lux-admin-eyebrow">用户管理</span><h1>全局设置</h1><p>注册邮件、默认权限、安全规则和数据保留。</p></div>
      </header>
      <nav className="lux-admin-user-section-nav" aria-label="用户全局设置">
        {settingsSections.map((item) => (
          <button key={item.id} className={item.id === section.id ? "is-active" : undefined} type="button" aria-current={item.id === section.id ? "page" : undefined} onClick={() => setSearchParams({ section: item.id })}>
            <span>{item.label}</span><small>{item.module}</small>
          </button>
        ))}
      </nav>
      {section.id === "registration" ? <AdminUserRegistrationSettings /> : section.id === "mail" ? <AdminUserMailSettings /> : section.id === "retention" ? <AdminUserRetentionSettings /> : section.id === "defaults" ? <AdminUserDefaultPermissionsSettings /> : section.id === "presets" ? <AdminUserPermissionPresetsSettings /> : section.id === "blocklist" ? <AdminUserGlobalAccessRulesPanel /> : (
        <section className="lux-admin-user-module-placeholder" aria-labelledby="lux-admin-user-settings-title">
          <span className="lux-admin-user-module-icon"><SlidersHorizontal size={19} /></span>
          <div><span className="lux-admin-eyebrow">{section.module}</span><h2 id="lux-admin-user-settings-title">{section.label}</h2><p>全局设置结构已预留，配置和保存能力将在对应任务中实现。</p></div>
        </section>
      )}
    </div>
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
      <button className="lux-button lux-button-secondary" type="button" onClick={() => void settings.refetch()} disabled={settings.isFetching}>重新加载注册设置</button>
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
      <div className="lux-admin-panel-heading"><div><span className="lux-admin-eyebrow">模块二</span><h2 id="lux-admin-registration-title">注册与邀请</h2></div></div>
      <p className="lux-admin-muted">默认采用邀请制，管理员也可以直接创建账户。关闭公开注册不会撤销仍有效的已发邀请。</p>
      <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit} aria-busy={update.isPending} aria-describedby="lux-admin-registration-help">
        <label>注册模式<select aria-label="注册模式" value={mode} disabled={update.isPending} onChange={(event) => { setDraft(event.target.value as RegistrationMode); setNotice(""); update.reset(); }}>
          <option value="INVITATION_ONLY">仅邀请注册</option><option value="PUBLIC_EMAIL">公开邮箱注册</option>
        </select></label>
        <p id="lux-admin-registration-help" className="lux-admin-muted">公开申请需先验证邮箱，再等待管理员审批。账户权限由管理员分配，申请者不能自行选择权限。</p>
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-primary" type="submit" disabled={!changed || update.isPending}>{update.isPending ? "保存中…" : "保存注册模式"}</button>
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
    return <div role="alert" className="lux-error-copy">读取 SMTP 设置失败：{settingsQuery.error?.message || "没有收到设置数据"}<button className="lux-button lux-button-secondary" type="button" onClick={() => void settingsQuery.refetch()}>重试</button></div>;
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

  function removeCredentials() {
    if (!window.confirm("删除已保存的 SMTP 用户名和密码？")) return;
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
        <div className="lux-admin-panel-heading"><div><span className="lux-admin-eyebrow">模块二</span><h2 id="lux-admin-mail-settings-title">SMTP 服务</h2></div></div>
        <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={saveMailSettings}>
          <label>SMTP 主机<input aria-label="SMTP 主机" value={draft.host} onChange={(event) => { setDraft({ ...draft, host: event.target.value }); setSettingsNotice(""); }} autoComplete="off" maxLength={253} required disabled={saving} /></label>
          <label>SMTP 端口<input aria-label="SMTP 端口" type="number" min={1} max={65_535} step={1} value={draft.port} onChange={(event) => { setDraft({ ...draft, port: event.target.value }); setSettingsNotice(""); }} required disabled={saving} /></label>
          <label>传输加密<select aria-label="SMTP TLS 模式" value={draft.tlsMode} onChange={(event) => { setDraft({ ...draft, tlsMode: event.target.value as SmtpTlsMode }); setSettingsNotice(""); }} disabled={saving}><option value="SMTPS">隐式 TLS（SMTPS）</option><option value="STARTTLS">强制 STARTTLS</option></select></label>
          <label>发件人名称<input aria-label="SMTP 发件人名称" value={draft.senderName} onChange={(event) => { setDraft({ ...draft, senderName: event.target.value }); setSettingsNotice(""); }} maxLength={128} required disabled={saving} /></label>
          <label>发件人邮箱<input aria-label="SMTP 发件人邮箱" type="email" value={draft.senderEmail} onChange={(event) => { setDraft({ ...draft, senderEmail: event.target.value }); setSettingsNotice(""); }} maxLength={320} autoComplete="email" required disabled={saving} /></label>
          <label>站点访问地址<input aria-label="SMTP 站点访问地址" type="url" value={draft.siteUrl} onChange={(event) => { setDraft({ ...draft, siteUrl: event.target.value }); setSettingsNotice(""); }} placeholder="https://lux.example.com" required disabled={saving} /></label>
          <p className="lux-admin-muted">仅支持 SMTPS 或强制 STARTTLS。此地址用于后续邮件中的验证链接。</p>
          <div className="lux-admin-user-profile-actions">
            <button className="lux-button lux-button-primary" type="submit" disabled={!validSettings || !settingsChanged || saving}>{saving ? "保存中…" : "保存 SMTP 设置"}</button>
            {settingsNotice ? <span role={saveSettings.isError ? "alert" : "status"}>{saveSettings.error?.message || settingsNotice}</span> : null}
          </div>
        </form>
      </section>

      <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-mail-credentials-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-mail-credentials-title">SMTP 身份验证</h2></div></div>
        <p className="lux-admin-muted" role="status">{credentialsConfigured ? "SMTP 凭据已配置；当前用户名和密码不会显示。" : "SMTP 凭据未配置；如邮件服务器需要认证，请输入用户名和密码。"}</p>
        <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submitCredentials}>
          <label>SMTP 用户名<input aria-label="SMTP 用户名" value={username} onChange={(event) => { setUsername(event.target.value); setCredentialsNotice(""); setCredentialsError(false); }} autoComplete="username" maxLength={320} disabled={credentialBusy} /></label>
          <label>SMTP 密码<input aria-label="SMTP 密码" type="password" value={password} onChange={(event) => { setPassword(event.target.value); setCredentialsNotice(""); setCredentialsError(false); }} autoComplete="new-password" maxLength={4_096} disabled={credentialBusy} /></label>
          {hasCredentialInput && !validCredentialPair ? <p className="lux-admin-playback-validation" role="alert">请同时填写 SMTP 用户名和密码。</p> : null}
          <div className="lux-admin-user-profile-actions">
            <button className="lux-button lux-button-primary" type="submit" disabled={!validCredentialPair || credentialBusy}>{savingCredentials ? "保存中…" : "保存 SMTP 凭据"}</button>
            {credentialsConfigured ? <button className="lux-button lux-button-secondary" type="button" onClick={removeCredentials} disabled={credentialBusy}>{deleteCredentials.isPending ? "删除中…" : "删除 SMTP 凭据"}</button> : null}
            {credentialsNotice ? <span role={credentialsError || deleteCredentials.isError ? "alert" : "status"}>{deleteCredentials.error?.message || credentialsNotice}</span> : null}
          </div>
        </form>
      </section>

      <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-mail-connection-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-mail-connection-title">连接检查</h2></div></div>
        <p className="lux-admin-muted">连接检查使用最近一次保存的配置，通过 TLS 执行 SMTP NOOP，不会发送测试邮件；单次检查最多等待 8 秒。</p>
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-secondary" type="button" onClick={() => { setConnectionNotice(""); testConnection.mutate(); }} disabled={!settingsQuery.data.configured || settingsChanged || connectionBusy}>{connectionBusy ? "检查中…" : "检查 SMTP 连接"}</button>
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
            <input
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
            <button className="lux-button lux-button-primary" type="submit" disabled={!validDays || !hasChanges || update.isPending}>
              {update.isPending ? "保存中…" : "保存保留期限"}
            </button>
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
      <div className="lux-admin-panel-heading"><div><span className="lux-admin-eyebrow">模块七</span><h2 id="lux-admin-user-activity-retention-title">活动记录保留</h2></div></div>
      <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit}>
        <label>
          活动记录保留天数
          <input
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
          <button className="lux-button lux-button-primary" type="submit" disabled={!validDays || !hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存活动保留期限"}
          </button>
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
            <label key={mode}>
              <input type="radio" name="new-user-library-access" value={mode} checked={currentDraft.libraryAccessMode === mode} disabled={update.isPending} onChange={() => updateDraft({ libraryAccessMode: mode })} />
              <span><strong>{title}</strong><small>{description}</small></span>
            </label>
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
                  <label key={library.id}>
                    <input type="checkbox" value={library.id} checked={selectedLibraryIds.includes(library.id)} disabled={update.isPending} onChange={(event) => toggleLibrary(library.id, event.currentTarget.checked)} />
                    <span>{library.name}</span>
                  </label>
                ))}
              </div>
            ) : null}
          </>
        ) : null}
        <fieldset className="lux-admin-user-defaults-policy-options">
          <legend>下载与播放/转码</legend>
          <label><input type="checkbox" aria-label="允许下载" checked={currentDraft.canDownload} disabled={update.isPending} onChange={(event) => updateDraft({ canDownload: event.currentTarget.checked })} /><span>允许下载</span></label>
          <label><input type="checkbox" aria-label="允许播放" checked={currentDraft.canPlay} disabled={update.isPending} onChange={(event) => updateDraft({ canPlay: event.currentTarget.checked })} /><span>允许播放</span></label>
          <label><input type="checkbox" aria-label="允许封装转换" checked={currentDraft.allowRemux} disabled={update.isPending} onChange={(event) => updateDraft({ allowRemux: event.currentTarget.checked })} /><span>允许封装转换</span></label>
          <label><input type="checkbox" aria-label="允许音频转码" checked={currentDraft.allowAudioTranscode} disabled={update.isPending} onChange={(event) => updateDraft({ allowAudioTranscode: event.currentTarget.checked })} /><span>允许音频转码</span></label>
          <label><input type="checkbox" aria-label="允许视频转码" checked={currentDraft.allowVideoTranscode} disabled={update.isPending} onChange={(event) => updateDraft({ allowVideoTranscode: event.currentTarget.checked })} /><span>允许视频转码</span></label>
        </fieldset>
        <label className="lux-admin-user-playback-limit">
          同时播放上限（0 表示不限）
          <input type="number" aria-label="新用户同时播放上限" min={0} step={1} value={limitInput} disabled={update.isPending} onChange={(event) => { setLimitDraft(event.currentTarget.value); setNotice(""); setError(""); }} />
        </label>
        {!validLimit ? <p className="lux-admin-user-playback-validation">请输入大于或等于 0 的整数。</p> : null}
        <p className="lux-admin-muted">此处配置新账号的初始权限快照；修改不会自动改变已有用户。</p>
        <div className="lux-admin-user-profile-actions">
          <button className="lux-button lux-button-primary" type="submit" disabled={!validLimit || !hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存默认权限"}
          </button>
          {error ? <span role="alert">{error}</span> : notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

function AdminUserPermissionPresetsSettings() {
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
    setError(cause.message || "权限预设操作失败，请重试。");
  };
  const create = useMutation({
    mutationFn: (input: AdminUserPermissionPresetInput) => api.createAdminUserPermissionPreset(input),
    onSuccess: () => finishMutation("权限预设已创建。"),
    onError: showMutationError,
  });
  const update = useMutation({
    mutationFn: ({ presetId, version, input }: { presetId: string; version: number; input: AdminUserPermissionPresetInput }) =>
      api.updateAdminUserPermissionPreset(presetId, version, input),
    onSuccess: () => finishMutation("权限预设已更新。"),
    onError: showMutationError,
  });
  const remove = useMutation({
    mutationFn: ({ presetId, version }: { presetId: string; version: number }) =>
      api.deleteAdminUserPermissionPreset(presetId, version),
    onSuccess: () => finishMutation("权限预设已删除。"),
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

  function deletePreset(preset: AdminUserPermissionPreset) {
    if (busy || preset.isBuiltin) return;
    const accepted = window.confirm(`确定删除“${preset.name}”预设吗？已应用到用户的权限快照不会改变。`);
    if (!accepted) return;
    setError("");
    setNotice("");
    remove.mutate({ presetId: preset.id, version: preset.version });
  }

  if (presetsQuery.isPending || librariesQuery.isPending) {
    return <AdminUserManagementState label="正在读取权限预设…" />;
  }
  if (presetsQuery.error) {
    return <AdminUserManagementState label={`读取权限预设失败：${presetsQuery.error.message}`} error />;
  }
  if (librariesQuery.error) {
    return <AdminUserManagementState label={`读取媒体库列表失败：${librariesQuery.error.message}`} error />;
  }

  return (
    <section className="lux-admin-panel lux-admin-user-permissions lux-admin-user-presets" aria-labelledby="lux-admin-user-presets-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-presets-title">角色预设</h2><p className="lux-admin-muted">预设保存权限快照；之后修改不会改变已应用到用户的权限。</p></div>
        <button className="lux-button lux-button-primary" type="button" onClick={startCreate} disabled={busy}>新建角色预设</button>
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
                  <button className="lux-button lux-button-secondary" type="button" aria-label={`编辑预设 ${preset.name}`} onClick={() => startEdit(preset)} disabled={busy || preset.isBuiltin}>编辑</button>
                  <button className="lux-button lux-button-secondary" type="button" aria-label={`删除预设 ${preset.name}`} onClick={() => deletePreset(preset)} disabled={busy || preset.isBuiltin}>删除</button>
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
          <label>预设名称<input aria-label="预设名称" maxLength={80} value={draft.name} disabled={busy} onChange={(event) => updateDraft({ name: event.currentTarget.value })} /></label>
          <label>说明<input aria-label="预设说明" maxLength={500} value={draft.description ?? ""} disabled={busy} onChange={(event) => updateDraft({ description: event.currentTarget.value || null })} /></label>
          <fieldset className="lux-admin-user-defaults-policy-options">
            <legend>账号角色</legend>
            <label><input type="checkbox" aria-label="管理员角色" checked={draft.isAdmin} disabled={busy} onChange={(event) => updateDraft({ isAdmin: event.currentTarget.checked })} /><span>管理员角色</span></label>
            <label><input type="checkbox" aria-label="允许进入管理台" checked={draft.canManageServer} disabled={busy || !draft.isAdmin} onChange={(event) => updateDraft({ canManageServer: event.currentTarget.checked })} /><span>允许进入管理台</span></label>
          </fieldset>
          <fieldset className="lux-admin-user-access-modes">
            <legend>媒体库访问范围</legend>
            {([
              ["ALL", "全部媒体库"],
              ["SELECTED", "指定媒体库"],
              ["NONE", "无媒体库权限"],
            ] as const).map(([mode, title]) => (
              <label key={mode}><input type="radio" name="preset-library-access" value={mode} checked={draft.permissions.libraryAccessMode === mode} disabled={busy} onChange={() => updatePermissionDraft({ libraryAccessMode: mode, libraryIds: mode === "SELECTED" ? draft.permissions.libraryIds : [] })} /><span><strong>{title}</strong></span></label>
            ))}
          </fieldset>
          {draft.permissions.libraryAccessMode === "SELECTED" ? (
            <>
              {enabledLibraries.length ? (
                <div className="lux-admin-library-access">
                  {enabledLibraries.map((library) => (
                    <label key={library.id}><input type="checkbox" checked={selectedLibraryIds.includes(library.id)} disabled={busy} onChange={(event) => toggleLibrary(library.id, event.currentTarget.checked)} /><span>{library.name}</span></label>
                  ))}
                </div>
              ) : <p className="lux-admin-muted">当前没有已启用的媒体库；空选择保存后会设为无媒体库权限。</p>}
              {enabledLibraries.length > 0 && selectedLibraryIds.length === 0 ? <p className="lux-admin-muted">空选择保存后会设为无媒体库权限。</p> : null}
            </>
          ) : null}
          <fieldset className="lux-admin-user-defaults-policy-options">
            <legend>下载与播放/转码</legend>
            <label><input type="checkbox" aria-label="允许下载" checked={draft.permissions.canDownload} disabled={busy} onChange={(event) => updatePermissionDraft({ canDownload: event.currentTarget.checked })} /><span>允许下载</span></label>
            <label><input type="checkbox" aria-label="允许播放" checked={draft.permissions.canPlay} disabled={busy} onChange={(event) => updatePermissionDraft({ canPlay: event.currentTarget.checked })} /><span>允许播放</span></label>
            <label><input type="checkbox" aria-label="允许封装转换" checked={draft.permissions.allowRemux} disabled={busy} onChange={(event) => updatePermissionDraft({ allowRemux: event.currentTarget.checked })} /><span>允许封装转换</span></label>
            <label><input type="checkbox" aria-label="允许音频转码" checked={draft.permissions.allowAudioTranscode} disabled={busy} onChange={(event) => updatePermissionDraft({ allowAudioTranscode: event.currentTarget.checked })} /><span>允许音频转码</span></label>
            <label><input type="checkbox" aria-label="允许视频转码" checked={draft.permissions.allowVideoTranscode} disabled={busy} onChange={(event) => updatePermissionDraft({ allowVideoTranscode: event.currentTarget.checked })} /><span>允许视频转码</span></label>
          </fieldset>
          <label className="lux-admin-user-playback-limit">同时播放上限（0 表示不限）<input type="number" aria-label="预设同时播放上限" min={0} step={1} value={limitDraft} disabled={busy} onChange={(event) => { setLimitDraft(event.currentTarget.value); setError(""); setNotice(""); }} /></label>
          {!validName ? <p className="lux-admin-user-playback-validation">请输入 1–80 个字符的预设名称。</p> : null}
          {!validDescription ? <p className="lux-admin-user-playback-validation">说明不能超过 500 个字符。</p> : null}
          {!validLimit ? <p className="lux-admin-user-playback-validation">请输入大于或等于 0 的整数。</p> : null}
          <p className="lux-admin-muted">应用预设时会复制权限快照；以后修改预设不会自动改变已有用户。</p>
          <div className="lux-admin-user-profile-actions">
            <button className="lux-button lux-button-primary" type="submit" disabled={!canSave || busy}>{busy ? "保存中…" : "保存预设"}</button>
            <button className="lux-button lux-button-secondary" type="button" disabled={busy} onClick={() => { setDraft(null); setEditingPreset(null); setError(""); }}>取消</button>
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

function AdminUserManagementState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "无法打开用户详情" : "正在加载用户详情"}</h1><p>{label}</p><Link className="lux-button lux-button-secondary" to="/admin/users">返回用户列表</Link></section>;
}
