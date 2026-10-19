import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Check,
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
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { ApiError, api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AccountPreferences, AccountPreferencesUpdate, AuthSession, Library, LuxUser } from "../../lib/api/types";
import { LuxSelect } from "../../components/LuxSelect";
import { useAvatar } from "../../components/layout/LuxShell";
import { calculateAvatarCrop, cropAvatarImage, DEFAULT_AVATAR_CROP, type AvatarCrop } from "./avatar-image";
import {
  applyAccountTheme,
  applyAccountAccent,
  moveLibrary,
  orderLibraries,
  readAccountSettings,
  readStoredAccountSettings,
  saveAccountSettings,
  type AccountSettings,
} from "./account-settings";

export function AccountPage({ user }: { user: LuxUser }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const accountProfile = useQuery({ queryKey: queryKeys.accountProfile, queryFn: () => api.accountProfile() });
  const accountPreferences = useQuery({
    queryKey: queryKeys.accountPreferences(user.id),
    queryFn: () => api.accountPreferences(),
  });
  const libraries = useQuery({ queryKey: queryKeys.libraries, queryFn: () => api.libraries() });
  const playbackSettings = useQuery({ queryKey: queryKeys.userSettings, queryFn: () => api.userSettings() });
  const libraryOrder = useQuery({ queryKey: queryKeys.libraryOrder, queryFn: () => api.libraryOrder() });
  const { avatarUrl, setAvatarUrl } = useAvatar();
  const [settings, setSettings] = useState<AccountSettings>(() => readAccountSettings(user.id));
  const settingsRef = useRef(settings);
  const accountPreferencesVersion = useRef<number | null>(null);
  const accountPreferencesInitialized = useRef(false);
  const accountPreferencesWriteBlocked = useRef(false);
  const accountPreferencesWriteQueue = useRef<Promise<void>>(Promise.resolve());
  const pendingAccountPreferenceWrites = useRef(0);
  const [accountPreferencesReady, setAccountPreferencesReady] = useState(false);
  const [accountPreferencesSaving, setAccountPreferencesSaving] = useState(false);
  const [accountPreferencesBlocked, setAccountPreferencesBlocked] = useState(false);
  const [accountPreferencesNotice, setAccountPreferencesNotice] = useState<string | null>(null);
  const [pendingLibraryOrder, setPendingLibraryOrder] = useState<string[] | null>(null);
  const [draggedLibraryId, setDraggedLibraryId] = useState<string | null>(null);
  const [avatarImageFailed, setAvatarImageFailed] = useState(false);
  const [pendingAvatarUrl, setPendingAvatarUrl] = useState<string | null>(null);
  const [pendingAvatarFile, setPendingAvatarFile] = useState<File | null>(null);
  const [pendingAvatarDimensions, setPendingAvatarDimensions] = useState<{ width: number; height: number } | null>(null);
  const [avatarCrop, setAvatarCrop] = useState<AvatarCrop>(DEFAULT_AVATAR_CROP);
  const [avatarReading, setAvatarReading] = useState(false);
  const [avatarPreparing, setAvatarPreparing] = useState(false);
  const [avatarNotice, setAvatarNotice] = useState<string | null>(null);
  const [passwordNotice, setPasswordNotice] = useState<string | null>(null);
  const [libraryOrderNotice, setLibraryOrderNotice] = useState<string | null>(null);
  const legacyLibraryOrderMigrationAttempted = useRef(false);
  const [playedPercent, setPlayedPercent] = useState("95");
  const [playedPercentNotice, setPlayedPercentNotice] = useState<string | null>(null);
  const [profileName, setProfileName] = useState(user.displayName || user.usernameNormalized);
  const [profileEmail, setProfileEmail] = useState("");
  const [profileNotice, setProfileNotice] = useState<string | null>(null);
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");

  const queueAccountPreferencesSave = (snapshot: AccountSettings): Promise<AccountPreferences | null> => {
    pendingAccountPreferenceWrites.current += 1;
    setAccountPreferencesSaving(true);
    setAccountPreferencesNotice(null);
    const queuedWrite = accountPreferencesWriteQueue.current
      .catch(() => undefined)
      .then(async () => {
        if (accountPreferencesWriteBlocked.current) return null;
        const expectedVersion = accountPreferencesVersion.current;
        if (expectedVersion === null) throw new Error("账户偏好尚未加载");
        const saved = await api.updateAccountPreferences(accountPreferencesUpdateFor(snapshot, expectedVersion));
        accountPreferencesVersion.current = saved.version;
        queryClient.setQueryData(queryKeys.accountPreferences(user.id), saved);
        saveAccountSettings(accountSettingsFromPreferences(saved, settingsRef.current.libraryOrder), user.id);
        void queryClient.invalidateQueries({ queryKey: queryKeys.home });
        setAccountPreferencesNotice("账户偏好已同步到此账户");
        return saved;
      })
      .catch((error: unknown) => {
        if (error instanceof ApiError && error.code === "VERSION_CONFLICT") {
          accountPreferencesWriteBlocked.current = true;
          setAccountPreferencesBlocked(true);
          setAccountPreferencesNotice("偏好已在其他设备更新，请刷新账户设置后再保存");
          void accountPreferences.refetch();
        } else {
          setAccountPreferencesNotice(
            error instanceof Error ? `账户偏好保存失败：${error.message}` : "账户偏好保存失败，请重试。",
          );
        }
        return null;
      })
      .finally(() => {
        pendingAccountPreferenceWrites.current = Math.max(0, pendingAccountPreferenceWrites.current - 1);
        setAccountPreferencesSaving(pendingAccountPreferenceWrites.current > 0);
      });
    accountPreferencesWriteQueue.current = queuedWrite.then(() => undefined);
    return queuedWrite;
  };

  useEffect(() => {
    if (
      !accountPreferences.isSuccess
      || accountPreferences.isFetching
      || !accountPreferences.data
      || accountPreferencesInitialized.current
    ) return;
    accountPreferencesInitialized.current = true;
    accountPreferencesVersion.current = accountPreferences.data.version;
    const currentSettings = settingsRef.current;
    if (accountPreferences.data.version > 0) {
      const serverSettings = accountSettingsFromPreferences(accountPreferences.data, currentSettings.libraryOrder);
      settingsRef.current = serverSettings;
      setSettings(serverSettings);
      saveAccountSettings(serverSettings, user.id);
      setAccountPreferencesReady(true);
      return;
    }

    const legacySettings = readStoredAccountSettings(user.id);
    if (legacySettings) {
      const migratedSettings = { ...currentSettings, ...legacySettings };
      settingsRef.current = migratedSettings;
      setSettings(migratedSettings);
      void queueAccountPreferencesSave(migratedSettings).finally(() => {
        setAccountPreferencesReady(true);
      });
      return;
    }

    const serverDefaults = accountSettingsFromPreferences(accountPreferences.data, currentSettings.libraryOrder);
    settingsRef.current = serverDefaults;
    setSettings(serverDefaults);
    setAccountPreferencesReady(true);
  }, [accountPreferences.data, accountPreferences.isFetching, accountPreferences.isSuccess, user.id]);

  useEffect(() => {
    if (playbackSettings.data) setPlayedPercent(String(playbackSettings.data.playedPercent));
  }, [playbackSettings.data]);

  const orderedLibraries = useMemo(
    () => pendingLibraryOrder
      ? orderLibraries(libraries.data?.libraries ?? [], pendingLibraryOrder)
      : libraries.data?.libraries ?? [],
    [libraries.data?.libraries, pendingLibraryOrder],
  );
  const useAdminLibraryOrder = playbackSettings.data?.useAdminLibraryOrder ?? true;
  const libraryOrderForced = playbackSettings.data?.libraryOrderForced ?? false;
  const libraryOrderLocked = libraryOrderForced || (!user.canManageServer && useAdminLibraryOrder);
  const accountPreferencesDisabled = !accountPreferencesReady || accountPreferencesBlocked;

  useEffect(() => {
    if (!pendingLibraryOrder || !libraries.data?.libraries) return;
    const serverOrder = libraries.data.libraries.map((library) => library.id);
    if (serverOrder.length === pendingLibraryOrder.length
      && serverOrder.every((id, index) => id === pendingLibraryOrder[index])) {
      setPendingLibraryOrder(null);
    }
  }, [libraries.data?.libraries, pendingLibraryOrder]);

  useEffect(() => {
    if (!accountPreferencesReady) return;
    applyAccountTheme(settings.theme);
    applyAccountAccent(settings.accentColor);
  }, [accountPreferencesReady, settings.accentColor, settings.theme]);

  const logout = useMutation({
    mutationFn: () => api.logout(),
    onSuccess: () => {
      queryClient.clear();
      navigate("/login", { replace: true });
    },
  });

  const avatarUpload = useMutation({
    mutationFn: (file: File) => api.uploadAvatar(file),
    onSuccess: () => {
      setAvatarUrl(api.avatarUrl(String(Date.now())));
      setAvatarImageFailed(false);
      setPendingAvatarUrl(null);
      setPendingAvatarFile(null);
      setAvatarNotice("头像已保存");
    },
    onError: (error) => {
      setAvatarNotice(error instanceof Error ? `头像保存失败：${error.message}` : "头像保存失败，请重试。");
    },
  });

  const savePlaybackSettings = useMutation({
    mutationFn: () => api.updateUserSettings({ playedPercent: Number(playedPercent) }),
    onSuccess: (data) => {
      setPlayedPercent(String(data.playedPercent));
      setPlayedPercentNotice("播放阈值已保存");
      void queryClient.invalidateQueries({ queryKey: queryKeys.userSettings });
    },
    onError: (error) => setPlayedPercentNotice(error instanceof Error ? error.message : "播放阈值保存失败，请重试。"),
  });

  const saveLibraryOrder = useMutation({
    mutationFn: (libraryOrder: string[]) => api.updateLibraryOrder({ libraryOrder }),
    onSuccess: (data) => {
      setPendingLibraryOrder(data.libraryOrder);
      updateSettings({ libraryOrder: data.libraryOrder });
      setLibraryOrderNotice("媒体库顺序已保存");
      queryClient.setQueryData(queryKeys.libraryOrder, data);
      void queryClient.invalidateQueries({ queryKey: queryKeys.libraries });
      void queryClient.invalidateQueries({ queryKey: queryKeys.home });
    },
    onError: (error) => {
      setPendingLibraryOrder(null);
      setLibraryOrderNotice(error instanceof Error ? `媒体库顺序保存失败：${error.message}` : "媒体库顺序保存失败，请重试。");
    },
  });

  const saveLibraryOrderPreference = useMutation({
    mutationFn: (useAdminLibraryOrder: boolean) => api.updateUserSettings({ useAdminLibraryOrder }),
    onSuccess: (data) => {
      queryClient.setQueryData(queryKeys.userSettings, data);
      setPendingLibraryOrder(null);
      setLibraryOrderNotice(data.useAdminLibraryOrder ? "已切换为按照管理员顺序排序" : "已切换为使用个人媒体库顺序");
      void queryClient.invalidateQueries({ queryKey: queryKeys.libraryOrder });
      void queryClient.invalidateQueries({ queryKey: queryKeys.libraries });
      void queryClient.invalidateQueries({ queryKey: queryKeys.home });
    },
    onError: (error) => setLibraryOrderNotice(error instanceof Error ? `媒体库顺序设置失败：${error.message}` : "媒体库顺序设置失败，请重试。"),
  });

  const changePassword = useMutation({
    mutationFn: () => api.updatePassword({ currentPassword, newPassword }),
    onSuccess: () => {
      setCurrentPassword("");
      setNewPassword("");
      setConfirmPassword("");
      setPasswordNotice("密码已修改");
    },
    onError: (error) => setPasswordNotice(error instanceof Error ? `密码修改失败：${error.message}` : "密码修改失败，请重试。"),
  });

  const saveProfile = useMutation({
    mutationFn: () => api.updateAccountProfile({ displayName: profileName, email: profileEmail }),
    onSuccess: ({ user: updatedUser }) => {
      setProfileName(updatedUser.displayName || updatedUser.usernameNormalized);
      setProfileEmail(updatedUser.email ?? "");
      setProfileNotice("账户资料已保存");
      queryClient.setQueryData(queryKeys.accountProfile, { user: updatedUser });
      queryClient.setQueryData<AuthSession>(queryKeys.me, (current) => current
        ? { ...current, user: { ...current.user, ...updatedUser } }
        : current);
      void queryClient.invalidateQueries({ queryKey: queryKeys.me });
    },
    onError: (error) => setProfileNotice(error instanceof Error ? `资料保存失败：${error.message}` : "资料保存失败，请重试。"),
  });

  useEffect(() => {
    const profileUser = accountProfile.data?.user;
    if (!profileUser) return;
    setProfileName(profileUser.displayName || profileUser.usernameNormalized);
    setProfileEmail(profileUser.email ?? "");
  }, [accountProfile.data?.user]);

  useEffect(() => {
    if (
      legacyLibraryOrderMigrationAttempted.current
      || !libraryOrder.isSuccess
      || (libraryOrder.data?.libraryOrder.length ?? 0)
      || !settings.libraryOrder.length
      || (!user.canManageServer && useAdminLibraryOrder)
    ) return;
    legacyLibraryOrderMigrationAttempted.current = true;
    setPendingLibraryOrder(settings.libraryOrder);
    saveLibraryOrder.mutate(settings.libraryOrder);
  }, [libraryOrder.data?.libraryOrder, libraryOrder.isSuccess, saveLibraryOrder, settings.libraryOrder, useAdminLibraryOrder, user.canManageServer]);

  const updateSettings = (patch: Partial<AccountSettings>) => {
    const nextSettings = { ...settingsRef.current, ...patch };
    settingsRef.current = nextSettings;
    setSettings(nextSettings);
    if (Object.keys(patch).every((key) => key === "libraryOrder")) {
      saveAccountSettings(nextSettings, user.id);
      return;
    }
    if (accountPreferencesReady && !accountPreferencesBlocked) {
      void queueAccountPreferencesSave(nextSettings);
    }
  };

  const persistLibraryOrder = (libraryOrder: string[]) => {
    if (libraryOrderLocked) return;
    setLibraryOrderNotice(null);
    setPendingLibraryOrder(libraryOrder);
    updateSettings({ libraryOrder });
    saveLibraryOrder.mutate(libraryOrder);
  };

  const reorderLibrary = (libraryId: string, direction: "up" | "down") => {
    const index = orderedLibraries.findIndex((library) => library.id === libraryId);
    if (index === -1) return;
    persistLibraryOrder(moveLibrary(orderedLibraries.map((library) => library.id), index, direction));
  };

  const dropLibrary = (targetId: string) => {
    if (!draggedLibraryId || draggedLibraryId === targetId) return;
    const fromIndex = orderedLibraries.findIndex((library) => library.id === draggedLibraryId);
    const targetIndex = orderedLibraries.findIndex((library) => library.id === targetId);
    if (fromIndex === -1 || targetIndex === -1) return;
    const next = [...orderedLibraries.map((library) => library.id)];
    const [moved] = next.splice(fromIndex, 1);
    next.splice(targetIndex, 0, moved);
    persistLibraryOrder(next);
    setDraggedLibraryId(null);
  };

  const submitPasswordChange = (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setPasswordNotice(null);
    if (!currentPassword || !newPassword || !confirmPassword) {
      setPasswordNotice("请填写完整的密码");
      return;
    }
    if (newPassword !== confirmPassword) {
      setPasswordNotice("两次输入的新密码不一致");
      return;
    }
    changePassword.mutate();
  };

  const selectAvatar = (file: File | undefined) => {
    if (!file) return;
    setAvatarNotice(null);
    setPendingAvatarFile(file);
    setPendingAvatarDimensions(null);
    setAvatarCrop(DEFAULT_AVATAR_CROP);
    setAvatarReading(true);
    const reader = new FileReader();
    reader.onload = () => {
      const result = typeof reader.result === "string" ? reader.result : null;
      setPendingAvatarUrl(result);
      setAvatarReading(false);
      if (!result) {
        setPendingAvatarFile(null);
        setAvatarNotice("头像读取失败，请重试。");
      }
    };
    reader.onerror = () => {
      setPendingAvatarUrl(null);
      setPendingAvatarFile(null);
      setAvatarReading(false);
      setAvatarNotice("头像读取失败，请重试。");
    };
    reader.readAsDataURL(file);
  };

  const saveAvatar = async () => {
    if (!pendingAvatarFile || !pendingAvatarUrl || avatarReading || avatarPreparing || avatarUpload.isPending) return;
    setAvatarPreparing(true);
    try {
      avatarUpload.mutate(await cropAvatarImage(pendingAvatarFile, avatarCrop));
    } catch (error) {
      setAvatarNotice(error instanceof Error ? `头像处理失败：${error.message}` : "头像图片处理失败，请重试。");
    } finally {
      setAvatarPreparing(false);
    }
  };

  const displayName = user.displayName || user.usernameNormalized;
  const initials = displayName.slice(0, 1).toUpperCase();
  const displayedAvatarUrl = pendingAvatarUrl ?? (avatarImageFailed ? null : avatarUrl);
  const cropPreviewSize = 144;
  const cropPreviewStyle = pendingAvatarDimensions
    ? (() => {
      const crop = calculateAvatarCrop(pendingAvatarDimensions.width, pendingAvatarDimensions.height, avatarCrop);
      const scale = cropPreviewSize / crop.size;
      return {
        width: `${pendingAvatarDimensions.width * scale}px`,
        height: `${pendingAvatarDimensions.height * scale}px`,
        left: `${-crop.x * scale}px`,
        top: `${-crop.y * scale}px`,
      };
    })()
    : undefined;

  return (
    <section className="lux-page lux-account-page">
      <div className="lux-account-page-heading">
        <div>
          <h1>账户设置</h1>
          <p>管理你的观影偏好，让 Lux 更贴合你的使用习惯。</p>
        </div>
        <span className="lux-account-sync-status">
          <span aria-hidden="true" />
          {accountPreferencesSaving
            ? "正在同步账户偏好…"
            : accountPreferencesReady
              ? "账户偏好已同步 · 头像单独保存"
              : "正在读取账户偏好…"}
        </span>
      </div>

      <div className="lux-account-settings-grid">
        <aside className="lux-account-settings-sidebar" aria-label="账户设置导航">
          <div className="lux-account-profile-card">
            <div className="lux-settings-avatar" aria-hidden="true">
              {displayedAvatarUrl ? <img src={displayedAvatarUrl} alt="" onError={() => setAvatarImageFailed(true)} /> : initials}
            </div>
            <div>
              <strong>{displayName}</strong>
              <span>{user.usernameNormalized}</span>
            </div>
            <button
              className="lux-button lux-button-compact lux-button-secondary lux-account-logout-button"
              type="button"
              onClick={() => logout.mutate()}
              disabled={logout.isPending}
            >
              <LogOut size={15} />{logout.isPending ? "正在退出…" : "退出登录"}
            </button>
          </div>
          <nav className="lux-account-settings-nav">
            <a href="#appearance"><Palette size={16} />外观</a>
            <a href="#home-layout"><Monitor size={16} />首页排版</a>
            <a href="#playback"><PlayCircle size={16} />播放</a>
            <a href="#account"><UserRound size={16} />账户</a>
          </nav>
        </aside>

        <div className="lux-account-settings-content">
          {accountPreferences.error && !accountPreferencesReady ? (
            <p className="lux-error-copy" role="alert">
              账户偏好读取失败：{accountPreferences.error.message}{" "}
              <button type="button" onClick={() => void accountPreferences.refetch()}>重试</button>
            </p>
          ) : null}
          {accountPreferencesNotice ? (
            <p className={accountPreferencesBlocked || accountPreferencesNotice.startsWith("账户偏好保存失败") ? "lux-error-copy" : "lux-account-notice"}
              role={accountPreferencesBlocked || accountPreferencesNotice.startsWith("账户偏好保存失败") ? "alert" : "status"}>
              {accountPreferencesNotice}
            </p>
          ) : null}
          <SettingsSection id="appearance" icon={<Palette size={18} />} title="主题">
            <div className="lux-setting-row lux-theme-row">
              <div>
                <strong>界面主题</strong>
                <p>选择 Lux 的显示方式，此偏好会随账户同步。</p>
              </div>
              <div className="lux-theme-options" role="group" aria-label="界面主题">
                <button
                  className={settings.theme === "light" ? "is-selected" : ""}
                  type="button"
                  aria-label="切换到浅色模式"
                  aria-pressed={settings.theme === "light"}
                  disabled={accountPreferencesDisabled}
                  onClick={() => updateSettings({ theme: "light" })}
                >
                  <Sun size={16} />浅色
                </button>
                <button
                  className={settings.theme === "dark" ? "is-selected" : ""}
                  type="button"
                  aria-label="切换到深色模式"
                  aria-pressed={settings.theme === "dark"}
                  disabled={accountPreferencesDisabled}
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
                <AccentOption color="silver" label="银灰" selected={settings.accentColor === "silver"} disabled={accountPreferencesDisabled} onSelect={() => updateSettings({ accentColor: "silver" })} />
                <AccentOption color="berry" label="莓果" selected={settings.accentColor === "berry"} disabled={accountPreferencesDisabled} onSelect={() => updateSettings({ accentColor: "berry" })} />
                <AccentOption color="ocean" label="海蓝" selected={settings.accentColor === "ocean"} disabled={accountPreferencesDisabled} onSelect={() => updateSettings({ accentColor: "ocean" })} />
                <AccentOption color="amber" label="琥珀" selected={settings.accentColor === "amber"} disabled={accountPreferencesDisabled} onSelect={() => updateSettings({ accentColor: "amber" })} />
                <AccentOption color="mint" label="薄荷" selected={settings.accentColor === "mint"} disabled={accountPreferencesDisabled} onSelect={() => updateSettings({ accentColor: "mint" })} />
              </div>
            </div>
          </SettingsSection>

          <SettingsSection id="home-layout" icon={<Monitor size={18} />} title="首页排版">
            <div className="lux-setting-block">
              <div className="lux-setting-block-heading">
                <div>
                  <strong>媒体库顺序</strong>
                  <p>拖动卡片调整首页媒体库的显示顺序，也可以使用右侧箭头。</p>
                </div>
                <span className="lux-setting-hint">{libraryOrderLocked ? "由管理员控制" : "可拖拽排序"}</span>
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
                      draggable={!libraryOrderLocked}
                      onDragStart={() => { if (!libraryOrderLocked) setDraggedLibraryId(library.id); }}
                      onDragEnd={() => setDraggedLibraryId(null)}
                      onDragOver={(event) => event.preventDefault()}
                      onDrop={() => { if (!libraryOrderLocked) dropLibrary(library.id); }}
                    >
                      <GripVertical className="lux-drag-handle" size={17} aria-hidden="true" />
                      <div className="lux-account-library-index" aria-hidden="true">{String(index + 1).padStart(2, "0")}</div>
                      <div className="lux-account-library-copy"><strong>{library.name}</strong><span>{libraryKindLabel(library.kind)}</span></div>
                      <div className="lux-account-library-actions">
                        <button type="button" aria-label={`上移媒体库 ${library.name}`} disabled={libraryOrderLocked || index === 0} onClick={() => reorderLibrary(library.id, "up")}><ChevronUp size={16} /></button>
                        <button type="button" aria-label={`下移媒体库 ${library.name}`} disabled={libraryOrderLocked || index === orderedLibraries.length - 1} onClick={() => reorderLibrary(library.id, "down")}><ChevronDown size={16} /></button>
                      </div>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="lux-account-empty">还没有可排序的媒体库。</div>
              )}
              {libraryOrderNotice ? <p className="lux-account-notice" role="status">{libraryOrderNotice}</p> : null}
            </div>
            <div className="lux-setting-divider" />
            <ToggleRow
              title="按照管理员顺序排序"
              description={libraryOrderForced ? "服务器已强制使用管理员保存的媒体库顺序，当前设置不可取消。" : "让首页和媒体库入口使用管理员保存的媒体库顺序。"}
              checked={useAdminLibraryOrder}
              disabled={libraryOrderForced}
              onChange={(checked) => saveLibraryOrderPreference.mutate(checked)}
            />
            <ToggleRow title="显示媒体库区块" description="在首页展示你有权限访问的媒体库。" checked={settings.showMediaLibraries} disabled={accountPreferencesDisabled} onChange={(checked) => updateSettings({ showMediaLibraries: checked })} />
            <ToggleRow title="显示继续观看区块" description="在首页保留最近播放但尚未看完的内容。" checked={settings.showContinueWatching} disabled={accountPreferencesDisabled} onChange={(checked) => updateSettings({ showContinueWatching: checked })} />
          </SettingsSection>

          <SettingsSection id="playback" icon={<PlayCircle size={18} />} title="播放">
            <div className="lux-setting-form-grid">
              <label className="lux-setting-field">
                <span>默认音轨语言</span>
                <LuxSelect
                  value={settings.audioLanguage}
                  options={["原始音轨", "简体中文", "English", "日本語"].map((language) => ({ value: language, label: language }))}
                  onChange={(audioLanguage) => updateSettings({ audioLanguage })}
                  disabled={accountPreferencesDisabled}
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
                  disabled={accountPreferencesDisabled}
                  aria-label="默认字幕语言"
                />
                <small>没有匹配轨道时由播放器决定回退策略。</small>
              </label>
            </div>
            <div className="lux-setting-divider" />
            <ToggleRow title="自动播放下一集" description="一集结束后自动开始播放下一集。" checked={settings.autoPlayNextEpisode} disabled={accountPreferencesDisabled} onChange={(checked) => updateSettings({ autoPlayNextEpisode: checked })} />
            <div className="lux-setting-divider" />
            <div className="lux-setting-row">
              <div>
                <strong>自动标记已看</strong>
                <p>播放达到此百分比后，当前电影或单集会自动标记为已看。默认 95%。</p>
              </div>
              <form className="lux-account-playback-threshold" onSubmit={(event) => { event.preventDefault(); setPlayedPercentNotice(null); savePlaybackSettings.mutate(); }}>
                <div className="lux-admin-input-with-suffix">
                  <input aria-label="自动标记已看百分比" type="number" min="1" max="100" value={playedPercent} onChange={(event) => { setPlayedPercentNotice(null); setPlayedPercent(event.target.value); }} />
                  <em>%</em>
                </div>
                <button className="lux-button lux-button-compact lux-button-secondary" type="submit" disabled={savePlaybackSettings.isPending || playbackSettings.isPending}>
                  {savePlaybackSettings.isPending ? "保存中…" : "保存"}
                </button>
                {playedPercentNotice ? <span role="status">{playedPercentNotice}</span> : null}
              </form>
            </div>
          </SettingsSection>

          <SettingsSection id="account" icon={<UserRound size={18} />} title="账户">
            <div className="lux-account-profile-editor">
              <div className="lux-settings-avatar lux-settings-avatar-large">
                {displayedAvatarUrl ? <img src={displayedAvatarUrl} alt={`${displayName} 的头像`} onError={() => setAvatarImageFailed(true)} /> : <UserRound size={27} />}
              </div>
              <div>
                <strong>头像</strong>
                <p>使用 JPG、PNG 或 WebP 图片，可调整圆形头像中的取景位置和大小。</p>
                <div className="lux-account-avatar-actions">
                  <label className="lux-upload-button">
                    <span>{displayedAvatarUrl ? "更换头像" : "选择头像"}</span>
                    <input
                      type="file"
                      accept="image/jpeg,image/png,image/webp"
                      onChange={(event) => {
                        selectAvatar(event.target.files?.[0]);
                        event.currentTarget.value = "";
                      }}
                    />
                  </label>
                  <button
                    className="lux-button lux-button-compact lux-button-secondary"
                    type="button"
                    onClick={saveAvatar}
                    disabled={!pendingAvatarFile || !pendingAvatarUrl || avatarReading || avatarPreparing || avatarUpload.isPending}
                  >
                    {avatarUpload.isPending ? "保存中…" : avatarPreparing ? "裁切中…" : avatarReading ? "读取中…" : "保存头像"}
                  </button>
                </div>
                {pendingAvatarUrl ? (
                  <div className="lux-account-avatar-cropper">
                    <div className="lux-avatar-crop-preview" role="img" aria-label="头像裁切预览">
                      <img
                        src={pendingAvatarUrl}
                        alt=""
                        onLoad={(event) => setPendingAvatarDimensions({
                          width: event.currentTarget.naturalWidth,
                          height: event.currentTarget.naturalHeight,
                        })}
                        style={cropPreviewStyle}
                      />
                    </div>
                    <div className="lux-avatar-crop-controls">
                      <label>
                        <span>缩放 <output>{avatarCrop.zoom.toFixed(2)}×</output></span>
                        <input
                          type="range"
                          aria-label="头像缩放"
                          min="1"
                          max="3"
                          step="0.05"
                          value={avatarCrop.zoom}
                          onChange={(event) => setAvatarCrop((current) => ({ ...current, zoom: Number(event.target.value) }))}
                        />
                      </label>
                      <label>
                        <span>水平位置</span>
                        <input
                          type="range"
                          aria-label="头像水平位置"
                          min="-1"
                          max="1"
                          step="0.01"
                          value={avatarCrop.horizontal}
                          onChange={(event) => setAvatarCrop((current) => ({ ...current, horizontal: Number(event.target.value) }))}
                        />
                      </label>
                      <label>
                        <span>垂直位置</span>
                        <input
                          type="range"
                          aria-label="头像垂直位置"
                          min="-1"
                          max="1"
                          step="0.01"
                          value={avatarCrop.vertical}
                          onChange={(event) => setAvatarCrop((current) => ({ ...current, vertical: Number(event.target.value) }))}
                        />
                      </label>
                      <button
                        className="lux-avatar-crop-reset"
                        type="button"
                        aria-label="重置头像裁切"
                        onClick={() => setAvatarCrop(DEFAULT_AVATAR_CROP)}
                      >
                        重置
                      </button>
                      <small>保存后会生成透明边缘的圆形头像。</small>
                    </div>
                  </div>
                ) : null}
                {avatarNotice ? <p className="lux-account-notice" role="status">{avatarNotice}</p> : null}
              </div>
            </div>
            <div className="lux-setting-divider" />
            <form className="lux-setting-form-grid lux-account-form-grid" onSubmit={(event) => { event.preventDefault(); setProfileNotice(null); saveProfile.mutate(); }}>
              <label className="lux-setting-field"><span>显示名称</span><input value={profileName} onChange={(event) => setProfileName(event.target.value)} /></label>
              <label className="lux-setting-field"><span>账号</span><input value={user.usernameNormalized} readOnly /></label>
              <label className="lux-setting-field"><span>邮箱</span><input type="email" value={profileEmail} onChange={(event) => setProfileEmail(event.target.value)} autoComplete="email" placeholder="可选" /></label>
              <div className="lux-setting-field lux-account-profile-save">
                <span>邮箱状态</span>
                <small>{accountProfile.isPending ? "正在读取资料…" : accountProfile.data?.user.emailVerifiedAt ? "邮箱已验证" : profileEmail ? "邮箱未验证；邮件验证功能接入后可发送验证邮件。" : "尚未设置邮箱"}</small>
                <button className="lux-button lux-button-compact lux-button-secondary" type="submit" disabled={saveProfile.isPending || accountProfile.isPending}>{saveProfile.isPending ? "保存中…" : "保存资料"}</button>
              </div>
            </form>
            {accountProfile.error ? <p className="lux-error-copy" role="alert">账户资料读取失败：{accountProfile.error.message}</p> : null}
            {profileNotice ? <p className={saveProfile.error ? "lux-error-copy" : "lux-account-notice"} role={saveProfile.error ? "alert" : "status"}>{profileNotice}</p> : null}
            <form className="lux-password-panel" onSubmit={submitPasswordChange}>
              <input className="lux-visually-hidden" type="text" value={user.usernameNormalized} readOnly autoComplete="username" tabIndex={-1} aria-hidden="true" />
              <div className="lux-setting-block-heading"><div><strong>修改密码</strong><p>使用一个没有在其他服务重复使用的新密码。</p></div><ShieldCheck size={18} aria-hidden="true" /></div>
              <div className="lux-setting-form-grid lux-password-grid">
                <label className="lux-setting-field"><span>当前密码</span><input type="password" value={currentPassword} onChange={(event) => setCurrentPassword(event.target.value)} autoComplete="current-password" placeholder="输入当前密码" required /></label>
                <label className="lux-setting-field"><span>新密码</span><input type="password" value={newPassword} onChange={(event) => setNewPassword(event.target.value)} autoComplete="new-password" placeholder="输入新密码" required /></label>
                <label className="lux-setting-field"><span>确认新密码</span><input type="password" value={confirmPassword} onChange={(event) => setConfirmPassword(event.target.value)} autoComplete="new-password" placeholder="再次输入新密码" required /></label>
              </div>
              <button className="lux-button lux-button-compact lux-button-secondary" type="submit" disabled={changePassword.isPending}>{changePassword.isPending ? "保存中…" : "修改密码"}</button>
              {passwordNotice ? <p className="lux-account-notice" role="status">{passwordNotice}</p> : null}
            </form>
          </SettingsSection>

          {logout.error ? <p className="lux-error-copy">{logout.error.message}</p> : null}
        </div>
      </div>
    </section>
  );
}

function SettingsSection({ id, icon, title, children }: { id: string; icon: React.ReactNode; title: string; children: React.ReactNode }) {
  return (
    <section id={id} className="lux-account-settings-section">
      <div className="lux-account-settings-section-heading"><span className="lux-account-section-icon">{icon}</span><h2>{title}</h2></div>
      <div className="lux-account-settings-section-body">{children}</div>
    </section>
  );
}

function ToggleRow({ title, description, checked, disabled = false, onChange }: { title: string; description: string; checked: boolean; disabled?: boolean; onChange: (checked: boolean) => void }) {
  return (
    <label className="lux-setting-toggle-row">
      <span><strong>{title}</strong><small>{description}</small></span>
      <input type="checkbox" aria-label={title} checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
      <span className="lux-setting-switch" aria-hidden="true"><span /></span>
    </label>
  );
}

function AccentOption({ color, label, selected, disabled = false, onSelect }: { color: string; label: string; selected: boolean; disabled?: boolean; onSelect: () => void }) {
  return (
    <button className={`lux-accent-option is-${color}${selected ? " is-selected" : ""}`} type="button" aria-label={`选择强调色 ${label}`} aria-pressed={selected} disabled={disabled} onClick={onSelect}>
      <span className="lux-accent-swatch" aria-hidden="true" />
      <span>{label}</span>
      {selected ? <Check size={12} aria-hidden="true" /> : null}
    </button>
  );
}

function accountPreferencesUpdateFor(settings: AccountSettings, expectedVersion: number): AccountPreferencesUpdate {
  return {
    expectedVersion,
    theme: settings.theme,
    accentColor: settings.accentColor,
    showMediaLibraries: settings.showMediaLibraries,
    showContinueWatching: settings.showContinueWatching,
    audioLanguage: settings.audioLanguage,
    subtitleLanguage: settings.subtitleLanguage,
    autoPlayNextEpisode: settings.autoPlayNextEpisode,
  };
}

function accountSettingsFromPreferences(preferences: AccountPreferences, libraryOrder: string[]): AccountSettings {
  return {
    theme: preferences.theme,
    accentColor: preferences.accentColor,
    libraryOrder,
    showMediaLibraries: preferences.showMediaLibraries,
    showContinueWatching: preferences.showContinueWatching,
    audioLanguage: preferences.audioLanguage,
    subtitleLanguage: preferences.subtitleLanguage,
    autoPlayNextEpisode: preferences.autoPlayNextEpisode,
  };
}

function libraryKindLabel(kind: Library["kind"]): string {
  if (kind === "MOVIE") return "电影库";
  if (kind === "SERIES") return "剧集库";
  return "混合媒体库";
}
