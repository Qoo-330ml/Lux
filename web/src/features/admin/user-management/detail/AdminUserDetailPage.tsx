import { ArrowLeft } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { api } from "../../../../lib/api/client";
import { formatRelativeTime, formatTimestamp } from "../../users/format";
import { AdminUserManagementState } from "../AdminUserManagementState";
import { AdminUserActivityPanel } from "./ActivityPanel";
import { AdminUserContentRestrictionSection } from "./ContentRestrictionSection";
import { AdminUserLifecycleActions } from "./LifecycleActions";
import { AdminUserPasswordPanel } from "./PasswordPanel";
import { AdminUserPermissionsSection } from "./PermissionsSection";
import { AdminUserPlaybackPolicySection } from "./PlaybackPolicySection";
import { AdminUserPreferencesSection } from "./PreferencesPanel";
import { AdminUserProfileEditor } from "./ProfileEditor";
import { AdminUserSessionsSection } from "./sessions/SessionsSection";

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
