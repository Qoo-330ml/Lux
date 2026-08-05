import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ChevronDown,
  ChevronUp,
  GripVertical,
  LogOut,
  Monitor,
  Moon,
  Palette,
  PlayCircle,
  ShieldCheck,
  Sun,
  UserRound,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { Library, LuxUser } from "../../lib/api/types";
import { LuxSelect } from "../../components/LuxSelect";
import {
  applyAccountTheme,
  applyAccountAccent,
  moveLibrary,
  readAccountSettings,
  saveAccountSettings,
  type AccountSettings,
} from "./account-settings";

export function AccountPage({ user }: { user: LuxUser }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const libraries = useQuery({ queryKey: queryKeys.libraries, queryFn: () => api.libraries() });
  const [settings, setSettings] = useState<AccountSettings>(() => readAccountSettings(user.id));
  const [draggedLibraryId, setDraggedLibraryId] = useState<string | null>(null);
  const [avatarUrl, setAvatarUrl] = useState<string | null>(null);
  const [accountNotice, setAccountNotice] = useState<string | null>(null);
  const [profileName, setProfileName] = useState(user.displayName || user.usernameNormalized);

  const orderedLibraries = useMemo(
    () => orderLibraries(libraries.data?.libraries ?? [], settings.libraryOrder),
    [libraries.data?.libraries, settings.libraryOrder],
  );

  useEffect(() => {
    applyAccountTheme(settings.theme);
    applyAccountAccent(settings.accentColor);
    saveAccountSettings(settings, user.id);
  }, [settings, user.id]);

  useEffect(() => {
    if (!libraries.data?.libraries?.length) return;
    const ids = orderedLibraries.map((library) => library.id);
    if (ids.every((id, index) => settings.libraryOrder[index] === id) && ids.length === settings.libraryOrder.length) {
      return;
    }
    setSettings((current) => ({ ...current, libraryOrder: ids }));
  }, [libraries.data?.libraries, orderedLibraries, settings.libraryOrder]);

  useEffect(() => {
    return () => {
      if (avatarUrl) URL.revokeObjectURL(avatarUrl);
    };
  }, [avatarUrl]);

  const logout = useMutation({
    mutationFn: () => api.logout(),
    onSuccess: () => {
      queryClient.removeQueries({ queryKey: queryKeys.me });
      navigate("/login", { replace: true });
    },
  });

  const updateSettings = (patch: Partial<AccountSettings>) => {
    setSettings((current) => ({ ...current, ...patch }));
  };

  const reorderLibrary = (libraryId: string, direction: "up" | "down") => {
    const index = orderedLibraries.findIndex((library) => library.id === libraryId);
    if (index === -1) return;
    updateSettings({ libraryOrder: moveLibrary(orderedLibraries.map((library) => library.id), index, direction) });
  };

  const dropLibrary = (targetId: string) => {
    if (!draggedLibraryId || draggedLibraryId === targetId) return;
    const fromIndex = orderedLibraries.findIndex((library) => library.id === draggedLibraryId);
    const targetIndex = orderedLibraries.findIndex((library) => library.id === targetId);
    if (fromIndex === -1 || targetIndex === -1) return;
    const next = [...orderedLibraries.map((library) => library.id)];
    const [moved] = next.splice(fromIndex, 1);
    next.splice(targetIndex, 0, moved);
    updateSettings({ libraryOrder: next });
    setDraggedLibraryId(null);
  };

  const displayName = user.displayName || user.usernameNormalized;
  const initials = displayName.slice(0, 1).toUpperCase();

  return (
    <section className="lux-page lux-account-page">
      <div className="lux-account-page-heading">
        <div>
          <h1>账户设置</h1>
          <p>管理你的观影偏好，让 Lux 更贴合你的使用习惯。</p>
        </div>
        <span className="lux-account-sync-status"><span aria-hidden="true" />设置自动保存</span>
      </div>

      <div className="lux-account-settings-grid">
        <aside className="lux-account-settings-sidebar" aria-label="账户设置导航">
          <div className="lux-account-profile-card">
            <div className="lux-settings-avatar" aria-hidden="true">
              {avatarUrl ? <img src={avatarUrl} alt="" /> : initials}
            </div>
            <div>
              <strong>{displayName}</strong>
              <span>{user.usernameNormalized}</span>
            </div>
          </div>
          <nav className="lux-account-settings-nav">
            <a href="#appearance"><Palette size={16} />外观</a>
            <a href="#home-layout"><Monitor size={16} />首页排版</a>
            <a href="#playback"><PlayCircle size={16} />播放</a>
            <a href="#account"><UserRound size={16} />账户</a>
          </nav>
        </aside>

        <div className="lux-account-settings-content">
          <SettingsSection id="appearance" icon={<Palette size={18} />} eyebrow="APPEARANCE" title="主题">
            <div className="lux-setting-row lux-theme-row">
              <div>
                <strong>界面主题</strong>
                <p>选择 Lux 的显示方式，偏好会在这台设备上保留。</p>
              </div>
              <div className="lux-theme-options" role="group" aria-label="界面主题">
                <button
                  className={settings.theme === "light" ? "is-selected" : ""}
                  type="button"
                  aria-label="切换到浅色模式"
                  aria-pressed={settings.theme === "light"}
                  onClick={() => updateSettings({ theme: "light" })}
                >
                  <Sun size={16} />浅色
                </button>
                <button
                  className={settings.theme === "dark" ? "is-selected" : ""}
                  type="button"
                  aria-label="切换到深色模式"
                  aria-pressed={settings.theme === "dark"}
                  onClick={() => updateSettings({ theme: "dark" })}
                >
                  <Moon size={16} />深色
                </button>
              </div>
            </div>
            <div className="lux-setting-row lux-accent-row">
              <div>
                <strong>强调色</strong>
                <p>用于按钮、进度和选中状态的界面色彩。</p>
              </div>
              <div className="lux-accent-options" role="group" aria-label="界面强调色">
                <AccentOption color="berry" label="莓果" selected={settings.accentColor === "berry"} onSelect={() => updateSettings({ accentColor: "berry" })} />
                <AccentOption color="ocean" label="海蓝" selected={settings.accentColor === "ocean"} onSelect={() => updateSettings({ accentColor: "ocean" })} />
                <AccentOption color="amber" label="琥珀" selected={settings.accentColor === "amber"} onSelect={() => updateSettings({ accentColor: "amber" })} />
                <AccentOption color="mint" label="薄荷" selected={settings.accentColor === "mint"} onSelect={() => updateSettings({ accentColor: "mint" })} />
              </div>
            </div>
          </SettingsSection>

          <SettingsSection id="home-layout" icon={<Monitor size={18} />} eyebrow="HOME LAYOUT" title="首页排版">
            <div className="lux-setting-block">
              <div className="lux-setting-block-heading">
                <div>
                  <strong>媒体库顺序</strong>
                  <p>拖动卡片调整首页媒体库的显示顺序，也可以使用右侧箭头。</p>
                </div>
                <span className="lux-setting-hint">可拖拽排序</span>
              </div>
              {libraries.isPending ? (
                <div className="lux-account-library-list" aria-busy="true" aria-label="正在加载媒体库">
                  <div className="lux-account-library-skeleton" />
                  <div className="lux-account-library-skeleton" />
                </div>
              ) : libraries.error ? (
                <p className="lux-error-copy" role="alert">媒体库顺序暂时无法加载：{libraries.error.message}</p>
              ) : orderedLibraries.length ? (
                <div className="lux-account-library-list" role="list" aria-label="首页媒体库顺序">
                  {orderedLibraries.map((library, index) => (
                    <div
                      className="lux-account-library-row"
                      key={library.id}
                      role="listitem"
                      draggable
                      onDragStart={() => setDraggedLibraryId(library.id)}
                      onDragEnd={() => setDraggedLibraryId(null)}
                      onDragOver={(event) => event.preventDefault()}
                      onDrop={() => dropLibrary(library.id)}
                    >
                      <GripVertical className="lux-drag-handle" size={17} aria-hidden="true" />
                      <div className="lux-account-library-index" aria-hidden="true">{String(index + 1).padStart(2, "0")}</div>
                      <div className="lux-account-library-copy"><strong>{library.name}</strong><span>{libraryKindLabel(library.kind)}</span></div>
                      <div className="lux-account-library-actions">
                        <button type="button" aria-label={`上移媒体库 ${library.name}`} disabled={index === 0} onClick={() => reorderLibrary(library.id, "up")}><ChevronUp size={16} /></button>
                        <button type="button" aria-label={`下移媒体库 ${library.name}`} disabled={index === orderedLibraries.length - 1} onClick={() => reorderLibrary(library.id, "down")}><ChevronDown size={16} /></button>
                      </div>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="lux-account-empty">还没有可排序的媒体库。</div>
              )}
            </div>
            <div className="lux-setting-divider" />
            <ToggleRow title="显示媒体库区块" description="在首页展示你有权限访问的媒体库。" checked={settings.showMediaLibraries} onChange={(checked) => updateSettings({ showMediaLibraries: checked })} />
            <ToggleRow title="显示继续观看区块" description="在首页保留最近播放但尚未看完的内容。" checked={settings.showContinueWatching} onChange={(checked) => updateSettings({ showContinueWatching: checked })} />
          </SettingsSection>

          <SettingsSection id="playback" icon={<PlayCircle size={18} />} eyebrow="PLAYBACK" title="播放">
            <div className="lux-setting-form-grid">
              <label className="lux-setting-field">
                <span>默认音轨语言</span>
                <LuxSelect
                  value={settings.audioLanguage}
                  options={["原始音轨", "简体中文", "English", "日本語"].map((language) => ({ value: language, label: language }))}
                  onChange={(audioLanguage) => updateSettings({ audioLanguage })}
                  aria-label="默认音轨语言"
                />
                <small>播放时优先选择匹配的音频轨道。</small>
              </label>
              <label className="lux-setting-field">
                <span>默认字幕语言</span>
                <LuxSelect
                  value={settings.subtitleLanguage}
                  options={["关闭字幕", "简体中文", "繁體中文", "English"].map((language) => ({ value: language, label: language }))}
                  onChange={(subtitleLanguage) => updateSettings({ subtitleLanguage })}
                  aria-label="默认字幕语言"
                />
                <small>没有匹配轨道时由播放器决定回退策略。</small>
              </label>
            </div>
            <div className="lux-setting-divider" />
            <ToggleRow title="自动播放下一集" description="一集结束后自动开始播放下一集。" checked={settings.autoPlayNextEpisode} onChange={(checked) => updateSettings({ autoPlayNextEpisode: checked })} />
          </SettingsSection>

          <SettingsSection id="account" icon={<UserRound size={18} />} eyebrow="ACCOUNT" title="账户">
            <div className="lux-account-profile-editor">
              <div className="lux-settings-avatar lux-settings-avatar-large">
                {avatarUrl ? <img src={avatarUrl} alt={`${displayName} 的头像`} /> : <UserRound size={27} />}
              </div>
              <div>
                <strong>头像</strong>
                <p>使用 JPG、PNG 或 WebP 图片，建议使用正方形图片。</p>
                <label className="lux-upload-button">
                  <span>更换头像</span>
                  <input
                    type="file"
                    accept="image/jpeg,image/png,image/webp"
                    onChange={(event) => {
                      const file = event.target.files?.[0];
                      if (file) setAvatarUrl(URL.createObjectURL(file));
                    }}
                  />
                </label>
              </div>
            </div>
            <div className="lux-setting-divider" />
            <div className="lux-setting-form-grid lux-account-form-grid">
              <label className="lux-setting-field"><span>显示名称</span><input value={profileName} onChange={(event) => setProfileName(event.target.value)} /></label>
              <label className="lux-setting-field"><span>账号</span><input value={user.usernameNormalized} readOnly /></label>
            </div>
            <form className="lux-password-panel" onSubmit={(event) => { event.preventDefault(); setAccountNotice("账户资料和密码修改需要服务端账户接口，当前先保留设置入口。"); }}>
              <input className="lux-visually-hidden" type="text" value={user.usernameNormalized} readOnly autoComplete="username" tabIndex={-1} aria-hidden="true" />
              <div className="lux-setting-block-heading"><div><strong>修改密码</strong><p>使用一个没有在其他服务重复使用的新密码。</p></div><ShieldCheck size={18} aria-hidden="true" /></div>
              <div className="lux-setting-form-grid lux-password-grid">
                <label className="lux-setting-field"><span>当前密码</span><input type="password" autoComplete="current-password" placeholder="输入当前密码" /></label>
                <label className="lux-setting-field"><span>新密码</span><input type="password" autoComplete="new-password" placeholder="输入新密码" /></label>
                <label className="lux-setting-field"><span>确认新密码</span><input type="password" autoComplete="new-password" placeholder="再次输入新密码" /></label>
              </div>
              <button className="lux-button lux-button-secondary lux-account-password-button" type="submit">修改密码</button>
              {accountNotice ? <p className="lux-account-notice" role="status">{accountNotice}</p> : null}
            </form>
          </SettingsSection>

          <div className="lux-account-footer-card">
            <div><Monitor size={18} /><div><strong>当前设备</strong><span>Web 浏览器</span></div></div>
            <div><ShieldCheck size={18} /><div><strong>账户权限</strong><span>{user.canManageServer ? "服务器管理员" : "普通用户"}</span></div></div>
            <button className="lux-button lux-button-secondary" type="button" onClick={() => logout.mutate()} disabled={logout.isPending}><LogOut size={17} />{logout.isPending ? "正在退出…" : "退出登录"}</button>
          </div>
          {logout.error ? <p className="lux-error-copy">{logout.error.message}</p> : null}
        </div>
      </div>
    </section>
  );
}

function SettingsSection({ id, icon, eyebrow, title, children }: { id: string; icon: React.ReactNode; eyebrow: string; title: string; children: React.ReactNode }) {
  return (
    <section id={id} className="lux-account-settings-section">
      <div className="lux-account-settings-section-heading"><span className="lux-account-section-icon">{icon}</span><div><span className="lux-eyebrow">{eyebrow}</span><h2>{title}</h2></div></div>
      <div className="lux-account-settings-section-body">{children}</div>
    </section>
  );
}

function ToggleRow({ title, description, checked, onChange }: { title: string; description: string; checked: boolean; onChange: (checked: boolean) => void }) {
  return (
    <label className="lux-setting-toggle-row">
      <span><strong>{title}</strong><small>{description}</small></span>
      <input type="checkbox" checked={checked} onChange={(event) => onChange(event.target.checked)} />
      <span className="lux-setting-switch" aria-hidden="true"><span /></span>
    </label>
  );
}

function AccentOption({ color, label, selected, onSelect }: { color: string; label: string; selected: boolean; onSelect: () => void }) {
  return (
    <button className={`lux-accent-option is-${color}${selected ? " is-selected" : ""}`} type="button" aria-label={`选择强调色 ${label}`} aria-pressed={selected} onClick={onSelect}>
      <span className="lux-accent-swatch" aria-hidden="true" />
      <span>{label}</span>
    </button>
  );
}

function orderLibraries(libraries: Library[], savedOrder: string[]): Library[] {
  const positions = new Map(savedOrder.map((id, index) => [id, index]));
  return [...libraries].sort((left, right) => (positions.get(left.id) ?? Number.MAX_SAFE_INTEGER) - (positions.get(right.id) ?? Number.MAX_SAFE_INTEGER));
}

function libraryKindLabel(kind: Library["kind"]): string {
  if (kind === "MOVIE") return "电影库";
  if (kind === "SERIES") return "剧集库";
  return "混合媒体库";
}
