import { useQuery } from "@tanstack/react-query";
import { api } from "../../../../lib/api/client";
import type { AccountPreferences, AdminUser } from "../../../../lib/api/types";
import { LuxButton } from "../../../../components/ui";

export function AdminUserPreferencesSection({ user }: { user: AdminUser }) {
  const preferencesQuery = useQuery({
    queryKey: ["admin", "user", user.id, "account-preferences"],
    queryFn: () => api.adminUserAccountPreferences(user.id),
  });
  const preferences = preferencesQuery.data;
  const librariesQuery = useQuery({
    queryKey: ["admin", "libraries"],
    queryFn: () => api.adminLibraries(),
    enabled: Boolean(preferences?.libraryOrder.length),
  });

  if (preferencesQuery.isPending) {
    return <section className="lux-admin-panel lux-admin-user-preferences-panel lux-admin-user-profile-panel" role="status" aria-busy="true">正在读取账户偏好…</section>;
  }
  if (preferencesQuery.error || !preferences) {
    return (
      <section className="lux-admin-panel lux-admin-user-preferences-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-preferences-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-preferences-title">账户偏好</h2></div></div>
        <p className="lux-admin-user-preferences-error" role="alert">{preferencesQuery.error instanceof Error ? preferencesQuery.error.message : "账户偏好读取失败，请重试。"}</p>
        <LuxButton tone="secondary" type="button" onClick={() => { void preferencesQuery.refetch(); }}>重试读取偏好</LuxButton>
      </section>
    );
  }

  const libraryNames = new Map((librariesQuery.data?.libraries ?? []).map((library) => [library.id, library.name]));
  const updatedAt = preferences.updatedAt == null
    ? "尚未保存过偏好"
    : new Date(preferences.updatedAt * 1000).toLocaleString();

  return (
    <section className="lux-admin-panel lux-admin-user-preferences-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-preferences-title">
      <div className="lux-admin-panel-heading">
        <div>
          <h2 id="lux-admin-user-preferences-title">账户偏好</h2>
          <p className="lux-admin-user-preferences-meta">版本 {preferences.version} · {updatedAt}</p>
        </div>
      </div>
      <p className="lux-admin-user-preferences-note">
        本页只读。管理员如需把其他账号的偏好应用到此账号，请在用户列表选择目标并使用“复制偏好”预览。
        {user.deletedAt != null ? " 回收站账号的偏好保持只读。" : ""}
      </p>
      <div className="lux-admin-user-preferences-groups">
        <section className="lux-admin-user-preferences-group" aria-labelledby="lux-admin-user-preferences-appearance">
          <h3 id="lux-admin-user-preferences-appearance">外观</h3>
          <dl className="lux-admin-user-preferences-values">
            <div><dt>主题</dt><dd>{preferences.theme === "light" ? "浅色" : "深色"}</dd></div>
            <div><dt>强调色</dt><dd>{accountAccentColorLabels[preferences.accentColor] ?? preferences.accentColor}</dd></div>
          </dl>
        </section>
        <section className="lux-admin-user-preferences-group" aria-labelledby="lux-admin-user-preferences-home">
          <h3 id="lux-admin-user-preferences-home">首页</h3>
          <dl className="lux-admin-user-preferences-values">
            <div><dt>媒体库分区</dt><dd>{preferences.showMediaLibraries ? "显示" : "隐藏"}</dd></div>
            <div><dt>继续观看</dt><dd>{preferences.showContinueWatching ? "显示" : "隐藏"}</dd></div>
            <div>
              <dt>媒体库顺序</dt>
              <dd>
                {preferences.libraryOrder.length ? (
                  <ol className="lux-admin-user-preferences-order" aria-label="媒体库排序">
                    {preferences.libraryOrder.map((libraryId) => <li key={libraryId}>{libraryNames.get(libraryId) ?? `未知媒体库（${libraryId}）`}</li>)}
                  </ol>
                ) : "未设置个人排序"}
              </dd>
            </div>
          </dl>
        </section>
        <section className="lux-admin-user-preferences-group" aria-labelledby="lux-admin-user-preferences-playback">
          <h3 id="lux-admin-user-preferences-playback">播放</h3>
          <dl className="lux-admin-user-preferences-values">
            <div><dt>默认音轨</dt><dd>{preferences.audioLanguage}</dd></div>
            <div><dt>默认字幕</dt><dd>{preferences.subtitleLanguage}</dd></div>
            <div><dt>自动连播</dt><dd>{preferences.autoPlayNextEpisode ? "开启" : "关闭"}</dd></div>
            <div><dt>播放完成阈值</dt><dd>{preferences.playedPercent}%</dd></div>
          </dl>
        </section>
      </div>
    </section>
  );
}

const accountAccentColorLabels: Record<AccountPreferences["accentColor"], string> = {
  silver: "银灰",
  berry: "莓果",
  ocean: "海蓝",
  amber: "琥珀",
  mint: "薄荷",
};
