import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, Download, Globe2, PackageOpen, RefreshCw, Save, Settings2, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminPlugin } from "../../lib/api/types";
import { LuxSelect } from "../../components/LuxSelect";
import "./plugin-library.css";

export function AdminPluginsPage() {
  const queryClient = useQueryClient();
  const [mode, setMode] = useState<"store" | "installed">("store");
  const plugins = useQuery({ queryKey: queryKeys.adminPlugins, queryFn: () => api.adminPlugins() });
  const installedPlugins = useQuery({ queryKey: queryKeys.adminInstalledPlugins, queryFn: () => api.adminInstalledPlugins() });
  const store = useQuery({ queryKey: queryKeys.adminPluginStore, queryFn: () => api.adminPluginStore() });
  const [storeUrl, setStoreUrl] = useState("");
  const [storeDialogOpen, setStoreDialogOpen] = useState(false);
  const storeDialogCloseRef = useRef<HTMLButtonElement>(null);
  const closeStoreDialog = useCallback(() => setStoreDialogOpen(false), []);
  const updateStore = useMutation({
    mutationFn: () => api.updateAdminPluginStore(storeUrl.trim()),
    onSuccess: () => {
      closeStoreDialog();
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPluginStore });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
    },
  });
  const install = useMutation({
    mutationFn: (pluginId: string) => api.installAdminPlugin(pluginId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });
  const upgrade = useMutation({
    mutationFn: (pluginId: string) => api.upgradeAdminPlugin(pluginId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });
  const uninstall = useMutation({
    mutationFn: (pluginId: string) => api.uninstallAdminPlugin(pluginId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });
  const toggleEnabled = useMutation({
    mutationFn: ({ pluginId, enabled }: { pluginId: string; enabled: boolean }) => api.updateAdminPluginEnabled(pluginId, enabled),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });

  useEffect(() => {
    if (store.data?.url) setStoreUrl(store.data.url);
  }, [store.data?.url]);

  useEffect(() => {
    if (!storeDialogOpen) return;
    storeDialogCloseRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeStoreDialog();
      }
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [closeStoreDialog, storeDialogOpen]);

  if (plugins.isPending || installedPlugins.isPending || store.isPending) return <AdminPluginsState label="正在读取插件库…" />;
  if (plugins.error || installedPlugins.error || store.error) return <AdminPluginsState label={plugins.error?.message || installedPlugins.error?.message || store.error?.message || "插件库加载失败"} error />;

  const items = (mode === "store" ? plugins.data.plugins : installedPlugins.data.plugins) ?? [];
  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading">
        <div><h1>插件库</h1><p>从插件商店安装经过校验的插件，再为媒体库选择刮削器。</p></div>
        <button className="lux-button lux-button-secondary lux-admin-plugin-store-trigger" type="button" aria-label="设置插件商店来源" aria-haspopup="dialog" aria-expanded={storeDialogOpen} onClick={() => setStoreDialogOpen(true)}><Globe2 size={15} /> 插件商店来源</button>
      </header>
      {storeDialogOpen ? (
        <div className="lux-admin-plugin-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) closeStoreDialog(); }}>
          <section className="lux-admin-plugin-dialog lux-admin-plugin-store-dialog" role="dialog" aria-modal="true" aria-labelledby="plugin-store-source-dialog-title">
            <div className="lux-admin-plugin-dialog-heading">
              <div><h2 id="plugin-store-source-dialog-title">插件商店来源</h2><p className="lux-admin-plugin-dialog-copy">填写插件目录地址；GitHub 仓库地址会自动读取其 main/index.json。</p></div>
              <button ref={storeDialogCloseRef} className="lux-icon-button lux-admin-plugin-dialog-close" type="button" aria-label="关闭插件商店来源设置" onClick={closeStoreDialog}><X size={17} /></button>
            </div>
            <form className="lux-admin-plugin-dialog-form" onSubmit={(event) => { event.preventDefault(); updateStore.mutate(); }}>
              <label htmlFor="lux-plugin-store-url">目录地址<input id="lux-plugin-store-url" type="url" value={storeUrl} onChange={(event) => setStoreUrl(event.target.value)} placeholder={store.data?.defaultUrl} required /></label>
              <div className="lux-admin-plugin-dialog-actions">
                <button className="lux-button lux-button-secondary" type="button" onClick={closeStoreDialog}>取消</button>
                <button className="lux-button lux-button-primary" type="submit" disabled={updateStore.isPending || !storeUrl.trim()}>{updateStore.isPending ? "保存中…" : "保存来源"}</button>
              </div>
              {updateStore.error ? <span className="lux-error-copy" role="alert">{updateStore.error.message}</span> : null}
            </form>
          </section>
        </div>
      ) : null}
      <nav className="lux-admin-plugin-tabs" aria-label="插件库视图">
        <button className={mode === "store" ? "is-active" : ""} type="button" aria-pressed={mode === "store"} onClick={() => setMode("store")}>插件商店<span>{plugins.data.total ?? plugins.data.plugins?.length ?? 0}</span></button>
        <button className={mode === "installed" ? "is-active" : ""} type="button" aria-pressed={mode === "installed"} onClick={() => setMode("installed")}>已安装管理<span>{installedPlugins.data.total ?? installedPlugins.data.plugins?.length ?? 0}</span></button>
      </nav>
      <section className="lux-admin-plugin-grid" aria-label="可用插件">
        {items.length === 0 ? <div className="lux-admin-empty"><PackageOpen size={24} /><h2>{mode === "store" ? "暂无可用插件" : "还没有已安装插件"}</h2><p>{mode === "store" ? "插件目录为空，请稍后重试。" : "从插件商店安装插件后，会在这里统一配置和管理。"}</p></div> : items.map((plugin) => <PluginCard key={plugin.id} plugin={plugin} installing={install.isPending && install.variables === plugin.id} installedManagement={mode === "installed"} toggling={toggleEnabled.isPending && toggleEnabled.variables?.pluginId === plugin.id} uninstalling={uninstall.isPending && uninstall.variables === plugin.id} upgrading={upgrade.isPending && upgrade.variables === plugin.id} onInstall={() => install.mutate(plugin.id)} onToggleEnabled={(enabled) => toggleEnabled.mutate({ pluginId: plugin.id, enabled })} onUninstall={() => uninstall.mutate(plugin.id)} onUpgrade={() => upgrade.mutate(plugin.id)} />)}
      </section>
      {install.error || uninstall.error || toggleEnabled.error || upgrade.error ? <p className="lux-error-copy" role="alert">{install.error?.message || uninstall.error?.message || toggleEnabled.error?.message || upgrade.error?.message}</p> : null}
    </div>
  );
}

function PluginCard({ plugin, installing, installedManagement, toggling, uninstalling, upgrading, onInstall, onToggleEnabled, onUninstall, onUpgrade }: { plugin: AdminPlugin; installing: boolean; installedManagement: boolean; toggling: boolean; uninstalling: boolean; upgrading: boolean; onInstall: () => void; onToggleEnabled: (enabled: boolean) => void; onUninstall: () => void; onUpgrade: () => void }) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [uninstallDialogOpen, setUninstallDialogOpen] = useState(false);
  const [apiKey, setApiKey] = useState("");
  const [apiKeyDirty, setApiKeyDirty] = useState(false);
  const [preferredLanguage, setPreferredLanguage] = useState("zh-CN");
  const [languageFallbackEnabled, setLanguageFallbackEnabled] = useState(false);
  const [titleAliasReplacementEnabled, setTitleAliasReplacementEnabled] = useState(false);
  const [fallbackLanguages, setFallbackLanguages] = useState<string[]>(["zh-SG", "zh-HK", "zh-TW"]);
  const [alternateApiEnabled, setAlternateApiEnabled] = useState(false);
  const [apiBaseUrlChoice, setApiBaseUrlChoice] = useState("official");
  const [customApiBaseUrl, setCustomApiBaseUrl] = useState("");
  const [libraryIds, setLibraryIds] = useState<string[]>([]);
  const [concurrency, setConcurrency] = useState(2);
  const [introWindowSeconds, setIntroWindowSeconds] = useState(180);
  const [creditsWindowSeconds, setCreditsWindowSeconds] = useState(180);
  const [matchThreshold, setMatchThreshold] = useState(80);
  const [existingInfoPolicy, setExistingInfoPolicy] = useState("SKIP");
  const [mediaInfoEnabled, setMediaInfoEnabled] = useState(true);
  const [thumbnailEnabled, setThumbnailEnabled] = useState(false);
  const [thumbnailPositionPercent, setThumbnailPositionPercent] = useState(30);
  const [writeSidecars, setWriteSidecars] = useState(true);
  const [schedule, setSchedule] = useState("0 3 * * *");
  const closeRef = useRef<HTMLButtonElement>(null);
  const uninstallCancelRef = useRef<HTMLButtonElement>(null);
  const isMediaInfo = plugin.id === "org.lux.strm-media-info";
  const isChapterSource = plugin.capabilities?.some((capability) => capability === "chapters.detect" || capability === "chapters.lookup") === true;
  const configField = plugin.configFields.find((field) => field.key === "apiKey");
  const preferredLanguageField = plugin.configFields.find((field) => field.key === "preferredLanguage");
  const fallbackEnabledField = plugin.configFields.find((field) => field.key === "languageFallbackEnabled");
  const titleAliasReplacementField = plugin.configFields.find((field) => field.key === "titleAliasReplacementEnabled");
  const fallbackLanguagesField = plugin.configFields.find((field) => field.key === "fallbackLanguages");
  const alternateApiField = plugin.configFields.find((field) => field.key === "alternateApiEnabled");
  const apiBaseUrlField = plugin.configFields.find((field) => field.key === "apiBaseUrl");
  const libraryIdsField = plugin.configFields.find((field) => field.key === "libraryIds");
  const concurrencyField = plugin.configFields.find((field) => field.key === "concurrency");
  const introWindowField = plugin.configFields.find((field) => field.key === "introWindowSeconds");
  const creditsWindowField = plugin.configFields.find((field) => field.key === "creditsWindowSeconds");
  const matchThresholdField = plugin.configFields.find((field) => field.key === "matchThreshold");
  const existingInfoPolicyField = plugin.configFields.find((field) => field.key === "existingInfoPolicy");
  const mediaInfoEnabledField = plugin.configFields.find((field) => field.key === "mediaInfoEnabled");
  const thumbnailEnabledField = plugin.configFields.find((field) => field.key === "thumbnailEnabled");
  const thumbnailPositionPercentField = plugin.configFields.find((field) => field.key === "thumbnailPositionPercent");
  const writeSidecarsField = plugin.configFields.find((field) => field.key === "writeSidecars");
  const scheduleField = plugin.configFields.find((field) => field.key === "schedule");
  const customApiBaseUrlOption = apiBaseUrlField?.options?.find((option) => option.label === "自定义")?.value ?? "custom";
  const canConfigure = plugin.installed && plugin.configurable && plugin.configFields.length > 0;
  const toggleBlockedByProvider = plugin.unavailableReason === "OTHER_IP_LOCATION_PLUGIN_INSTALLED";
  const closeDialog = useCallback(() => setOpen(false), []);
  const save = useMutation({
    mutationFn: () => isMediaInfo
      ? api.updateAdminPluginConfig(plugin.id, {
          libraryIds,
          concurrency,
          existingInfoPolicy,
          ...(mediaInfoEnabledField ? { mediaInfoEnabled } : {}),
          ...(thumbnailEnabledField ? { thumbnailEnabled } : {}),
          ...(thumbnailPositionPercentField ? { thumbnailPositionPercent } : {}),
          writeSidecars,
          ...(scheduleField ? { schedule: schedule.trim() } : {}),
        })
      : isChapterSource
        ? api.updateAdminPluginConfig(plugin.id, {
            concurrency,
            ...(introWindowField ? { introWindowSeconds } : {}),
            ...(creditsWindowField ? { creditsWindowSeconds } : {}),
            ...(matchThresholdField ? { matchThreshold } : {}),
            ...(scheduleField ? { schedule: schedule.trim() } : {}),
          })
        : api.updateAdminPluginConfig(plugin.id, {
          ...(apiKeyDirty ? { apiKey } : {}),
          preferredLanguage,
          languageFallbackEnabled,
          titleAliasReplacementEnabled,
          fallbackLanguages,
          alternateApiEnabled,
          apiBaseUrl: apiBaseUrlChoice === customApiBaseUrlOption
            ? customApiBaseUrl.trim()
            : apiBaseUrlField?.options?.find((option) => option.value === apiBaseUrlChoice)?.label ?? customApiBaseUrl.trim(),
        }),
    onSuccess: () => {
      setApiKey("");
      closeDialog();
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });
  const run = useMutation({
    mutationFn: () => api.runAdminPlugin(plugin.id),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "jobs"] }),
  });

  useEffect(() => {
    if (!open) return;
    closeRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeDialog();
      }
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [closeDialog, open]);

  useEffect(() => {
    if (!uninstallDialogOpen) return;
    uninstallCancelRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        setUninstallDialogOpen(false);
      }
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [uninstallDialogOpen]);

  useEffect(() => {
    if (!open) return;
    const values = plugin.configValues ?? {};
    const preferred = typeof values.preferredLanguage === "string"
      ? values.preferredLanguage
      : preferredLanguageField?.options?.[0]?.value ?? "zh-CN";
    const fallback = Array.isArray(values.fallbackLanguages)
      ? values.fallbackLanguages.filter((value): value is string => typeof value === "string")
      : ["zh-SG", "zh-HK", "zh-TW"];
    const configuredApiBaseUrl = typeof values.apiBaseUrl === "string"
      ? values.apiBaseUrl
      : apiBaseUrlField?.options?.[0]?.label ?? "https://api.themoviedb.org";
    const selectedApiOption = apiBaseUrlField?.options?.find(
      (option) => option.value !== customApiBaseUrlOption && option.label === configuredApiBaseUrl,
    );
    setPreferredLanguage(preferred);
    setLanguageFallbackEnabled(values.languageFallbackEnabled === true);
    setTitleAliasReplacementEnabled(values.titleAliasReplacementEnabled === true);
    setFallbackLanguages(fallback);
    setAlternateApiEnabled(values.alternateApiEnabled === true);
    setApiBaseUrlChoice(selectedApiOption?.value ?? customApiBaseUrlOption);
    setCustomApiBaseUrl(selectedApiOption ? "" : configuredApiBaseUrl);
    const configuredLibraryIds = Array.isArray(values.libraryIds)
      ? values.libraryIds.filter((value): value is string => typeof value === "string")
      : [];
    setLibraryIds(configuredLibraryIds);
    setConcurrency(typeof values.concurrency === "number" ? values.concurrency : Number(concurrencyField?.defaultValue ?? 2));
    setIntroWindowSeconds(typeof values.introWindowSeconds === "number" ? values.introWindowSeconds : Number(introWindowField?.defaultValue ?? 180));
    setCreditsWindowSeconds(typeof values.creditsWindowSeconds === "number" ? values.creditsWindowSeconds : Number(creditsWindowField?.defaultValue ?? 180));
    setMatchThreshold(typeof values.matchThreshold === "number" ? values.matchThreshold : Number(matchThresholdField?.defaultValue ?? 80));
    const configuredExistingInfoPolicy = typeof values.existingInfoPolicy === "string"
      ? values.existingInfoPolicy
      : String(existingInfoPolicyField?.defaultValue ?? "SKIP");
    const configuredThumbnailPositionPercent = typeof values.thumbnailPositionPercent === "number"
      ? values.thumbnailPositionPercent
      : Number(thumbnailPositionPercentField?.defaultValue ?? 30);
    setExistingInfoPolicy(configuredExistingInfoPolicy);
    setMediaInfoEnabled(values.mediaInfoEnabled !== false);
    setThumbnailEnabled(values.thumbnailEnabled === true);
    setThumbnailPositionPercent(configuredThumbnailPositionPercent);
    setWriteSidecars(values.writeSidecars !== false);
    setSchedule(typeof values.schedule === "string" ? values.schedule : String(scheduleField?.defaultValue ?? "0 3 * * *"));
    setApiKey("");
    setApiKeyDirty(false);
  }, [apiBaseUrlField?.options, concurrencyField?.defaultValue, creditsWindowField?.defaultValue, customApiBaseUrlOption, existingInfoPolicyField?.defaultValue, introWindowField?.defaultValue, matchThresholdField?.defaultValue, open, plugin.configValues, preferredLanguageField?.options, scheduleField?.defaultValue, thumbnailPositionPercentField?.defaultValue, titleAliasReplacementField?.defaultValue]);

  return (
    <article className="lux-admin-panel lux-admin-plugin-card">
      <div className="lux-admin-plugin-icon" aria-hidden="true"><PackageOpen size={22} /></div>
      <div className="lux-admin-plugin-content">
        <div className="lux-admin-plugin-heading-line">
          <h2>{plugin.name}</h2>
          <div className="lux-admin-plugin-meta" aria-label="插件版本和分类">
            <span className="lux-admin-plugin-version">{plugin.version ? `v${plugin.version}` : "版本未知"}</span>
            <span className="lux-admin-plugin-category">{pluginCategoryLabel(plugin.category)}</span>
          </div>
        </div>
        <p title={plugin.description}>{plugin.description}</p>
      </div>
      <div className="lux-admin-plugin-actions">
        {plugin.installed && installedManagement ? (
          <>
            <button className={`lux-admin-plugin-enable-switch${plugin.enabled ? " is-enabled" : ""}`} type="button" role="switch" aria-checked={plugin.enabled} aria-label={toggleBlockedByProvider ? `由其他插件停用 ${plugin.name}` : `${plugin.enabled ? "禁用" : "启用"} ${plugin.name}`} disabled={toggling || uninstalling || upgrading || toggleBlockedByProvider} onClick={() => onToggleEnabled(!plugin.enabled)}>
              <span className="lux-admin-plugin-enable-switch-track" aria-hidden="true"><span /></span>
              <span>{plugin.enabled ? "已启用" : "已禁用"}</span>
            </button>
            {plugin.updateAvailable ? <button className="lux-button lux-button-secondary" type="button" aria-label={`升级 ${plugin.name}`} disabled={upgrading || uninstalling} onClick={onUpgrade}><RefreshCw size={14} /> {upgrading ? "升级中…" : `升级到 v${plugin.availableVersion}`}</button> : null}
            <button className="lux-admin-plugin-uninstall-button" type="button" aria-label={`卸载 ${plugin.name}`} disabled={uninstalling || upgrading} onClick={() => setUninstallDialogOpen(true)}><Trash2 size={14} /> 卸载</button>
          </>
        ) : plugin.installed ? (
          <span className="lux-admin-plugin-install-status is-installed" role="status" aria-label="插件状态：已安装"><CheckCircle2 size={15} /> 已安装</span>
        ) : (
          <button className="lux-admin-plugin-install-status is-install" type="button" aria-label={`安装 ${plugin.name}`} disabled={installing} onClick={onInstall}><Download size={15} /> {installing ? "安装中…" : "安装"}</button>
        )}
        {isMediaInfo && plugin.installed && plugin.configured ? <button className="lux-button lux-button-secondary" type="button" aria-label="开始提取" onClick={() => run.mutate()} disabled={run.isPending}>{run.isPending ? "启动中…" : "开始提取"}</button> : null}
        {canConfigure ? <button className="lux-admin-plugin-config-button" type="button" aria-label={`配置 ${plugin.name}`} onClick={() => setOpen(true)}><Settings2 size={15} /> 配置</button> : null}
      </div>
      {open && canConfigure ? (
        <div className="lux-admin-plugin-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) closeDialog(); }}>
          <section className="lux-admin-plugin-dialog" role="dialog" aria-modal="true" aria-labelledby={`plugin-config-title-${plugin.id}`}>
            <div className="lux-admin-plugin-dialog-heading">
              <div><h2 id={`plugin-config-title-${plugin.id}`}>{plugin.name}</h2></div>
              <button ref={closeRef} className="lux-icon-button lux-admin-plugin-dialog-close" type="button" aria-label={`关闭 ${plugin.name}配置`} onClick={closeDialog}><X size={17} /></button>
            </div>
            <form className="lux-admin-plugin-dialog-form" autoComplete="off" onSubmit={(event) => { event.preventDefault(); save.mutate(); }}>
              {isMediaInfo ? <>
                {libraryIdsField ? <label htmlFor={"plugin-config-" + plugin.id + "-library-ids"}>{libraryIdsField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-library-ids"} multiple value={libraryIds} options={libraryIdsField.options ?? []} onChange={setLibraryIds} aria-label={libraryIdsField.label} /><small>{libraryIdsField.description}</small></label> : null}
                {concurrencyField ? <label htmlFor={"plugin-config-" + plugin.id + "-concurrency"}>{concurrencyField.label}<input id={"plugin-config-" + plugin.id + "-concurrency"} type="number" min={concurrencyField.minimum ?? 1} max={concurrencyField.maximum ?? 64} value={concurrency} onChange={(event) => setConcurrency(Number(event.target.value))} /><small>{concurrencyField.description}</small></label> : null}
                {introWindowField ? <label htmlFor={"plugin-config-" + plugin.id + "-intro-window"}>{introWindowField.label}<input id={"plugin-config-" + plugin.id + "-intro-window"} type="number" min={introWindowField.minimum ?? 15} max={introWindowField.maximum ?? 300} value={introWindowSeconds} onChange={(event) => setIntroWindowSeconds(Number(event.target.value))} /><small>{introWindowField.description}</small></label> : null}
                {creditsWindowField ? <label htmlFor={"plugin-config-" + plugin.id + "-credits-window"}>{creditsWindowField.label}<input id={"plugin-config-" + plugin.id + "-credits-window"} type="number" min={creditsWindowField.minimum ?? 15} max={creditsWindowField.maximum ?? 600} value={creditsWindowSeconds} onChange={(event) => setCreditsWindowSeconds(Number(event.target.value))} /><small>{creditsWindowField.description}</small></label> : null}
                {matchThresholdField ? <label htmlFor={"plugin-config-" + plugin.id + "-match-threshold"}>{matchThresholdField.label}<input id={"plugin-config-" + plugin.id + "-match-threshold"} type="number" min={matchThresholdField.minimum ?? 1} max={matchThresholdField.maximum ?? 100} value={matchThreshold} onChange={(event) => setMatchThreshold(Number(event.target.value))} /><small>{matchThresholdField.description}</small></label> : null}
                {existingInfoPolicyField ? <label htmlFor={"plugin-config-" + plugin.id + "-existing-info-policy"}>{existingInfoPolicyField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-existing-info-policy"} value={existingInfoPolicy} options={existingInfoPolicyField.options ?? []} onChange={setExistingInfoPolicy} aria-label={existingInfoPolicyField.label} /><small>{existingInfoPolicyField.description}</small></label> : null}
                {mediaInfoEnabledField ? <label className="lux-admin-plugin-toggle"><input type="checkbox" checked={mediaInfoEnabled} onChange={(event) => setMediaInfoEnabled(event.target.checked)} /> <span><strong>{mediaInfoEnabledField.label}</strong><small>{mediaInfoEnabledField.description}</small></span></label> : null}
                {thumbnailEnabledField ? <label className="lux-admin-plugin-toggle"><input type="checkbox" checked={thumbnailEnabled} onChange={(event) => setThumbnailEnabled(event.target.checked)} /> <span><strong>{thumbnailEnabledField.label}</strong><small>{thumbnailEnabledField.description}</small></span></label> : null}
                {thumbnailPositionPercentField ? <label htmlFor={"plugin-config-" + plugin.id + "-thumbnail-position-percent"}>{thumbnailPositionPercentField.label}<input id={"plugin-config-" + plugin.id + "-thumbnail-position-percent"} type="number" required={thumbnailPositionPercentField.required} min={thumbnailPositionPercentField.minimum ?? 1} max={thumbnailPositionPercentField.maximum ?? 99} value={thumbnailPositionPercent} onChange={(event) => setThumbnailPositionPercent(Number(event.target.value))} /><small>{thumbnailPositionPercentField.description}</small></label> : null}
                {writeSidecarsField ? <label className="lux-admin-plugin-toggle"><input type="checkbox" checked={writeSidecars} onChange={(event) => setWriteSidecars(event.target.checked)} /> <span><strong>{writeSidecarsField.label}</strong><small>{writeSidecarsField.description}</small></span></label> : null}
                {scheduleField ? <label htmlFor={"plugin-config-" + plugin.id + "-schedule"}>{scheduleField.label}<input id={"plugin-config-" + plugin.id + "-schedule"} type="text" required={scheduleField.required} value={schedule} onChange={(event) => setSchedule(event.target.value)} placeholder="0 3 * * *" /><small>{scheduleField.description}</small></label> : null}
              </> : isChapterSource ? <>
                {concurrencyField ? <label htmlFor={"plugin-config-" + plugin.id + "-concurrency"}>{concurrencyField.label}<input id={"plugin-config-" + plugin.id + "-concurrency"} type="number" min={concurrencyField.minimum ?? 1} max={concurrencyField.maximum ?? 64} value={concurrency} onChange={(event) => setConcurrency(Number(event.target.value))} /><small>{concurrencyField.description}</small></label> : null}
                {introWindowField ? <label htmlFor={"plugin-config-" + plugin.id + "-intro-window"}>{introWindowField.label}<input id={"plugin-config-" + plugin.id + "-intro-window"} type="number" min={introWindowField.minimum ?? 15} max={introWindowField.maximum ?? 300} value={introWindowSeconds} onChange={(event) => setIntroWindowSeconds(Number(event.target.value))} /><small>{introWindowField.description}</small></label> : null}
                {creditsWindowField ? <label htmlFor={"plugin-config-" + plugin.id + "-credits-window"}>{creditsWindowField.label}<input id={"plugin-config-" + plugin.id + "-credits-window"} type="number" min={creditsWindowField.minimum ?? 15} max={creditsWindowField.maximum ?? 600} value={creditsWindowSeconds} onChange={(event) => setCreditsWindowSeconds(Number(event.target.value))} /><small>{creditsWindowField.description}</small></label> : null}
                {matchThresholdField ? <label htmlFor={"plugin-config-" + plugin.id + "-match-threshold"}>{matchThresholdField.label}<input id={"plugin-config-" + plugin.id + "-match-threshold"} type="number" min={matchThresholdField.minimum ?? 1} max={matchThresholdField.maximum ?? 100} value={matchThreshold} onChange={(event) => setMatchThreshold(Number(event.target.value))} /><small>{matchThresholdField.description}</small></label> : null}
                {scheduleField ? <label htmlFor={"plugin-config-" + plugin.id + "-schedule"}>{scheduleField.label}<input id={"plugin-config-" + plugin.id + "-schedule"} type="text" required={scheduleField.required} value={schedule} onChange={(event) => setSchedule(event.target.value)} placeholder="0 3 * * *" /><small>{scheduleField.description}</small></label> : null}
              </> : <>
                {configField ? <label htmlFor={"plugin-config-" + plugin.id + "-api-key"}>{configField.label}<input id={"plugin-config-" + plugin.id + "-api-key"} type="password" value={apiKey} onChange={(event) => { setApiKey(event.target.value); setApiKeyDirty(true); }} placeholder="留空使用插件默认凭据" autoComplete="new-password" /></label> : null}
                {preferredLanguageField ? <label htmlFor={"plugin-config-" + plugin.id + "-preferred-language"}>{preferredLanguageField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-preferred-language"} value={preferredLanguage} options={preferredLanguageField.options ?? []} onChange={setPreferredLanguage} aria-label={preferredLanguageField.label} /></label> : null}
                {titleAliasReplacementField ? <label className="lux-admin-plugin-toggle"><input type="checkbox" checked={titleAliasReplacementEnabled} onChange={(event) => setTitleAliasReplacementEnabled(event.target.checked)} /> <span><strong>{titleAliasReplacementField.label}</strong><small>{titleAliasReplacementField.description}</small></span></label> : null}
                {fallbackEnabledField ? <label className="lux-admin-plugin-toggle"><input type="checkbox" checked={languageFallbackEnabled} onChange={(event) => setLanguageFallbackEnabled(event.target.checked)} /> <span><strong>{fallbackEnabledField.label}</strong><small>{fallbackEnabledField.description}</small></span></label> : null}
                {fallbackLanguagesField ? <label htmlFor={"plugin-config-" + plugin.id + "-fallback-languages"}>{fallbackLanguagesField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-fallback-languages"} multiple value={fallbackLanguages} options={fallbackLanguagesField.options ?? []} onChange={setFallbackLanguages} aria-label={fallbackLanguagesField.label} /><small>{fallbackLanguagesField.description}</small></label> : null}
                {alternateApiField ? <label className="lux-admin-plugin-toggle"><input type="checkbox" checked={alternateApiEnabled} onChange={(event) => setAlternateApiEnabled(event.target.checked)} /> <span><strong>{alternateApiField.label}</strong><small>{alternateApiField.description}</small></span></label> : null}
                {apiBaseUrlField ? <label htmlFor={"plugin-config-" + plugin.id + "-api-base-url"}>{apiBaseUrlField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-api-base-url"} value={apiBaseUrlChoice} options={apiBaseUrlField.options ?? []} disabled={!alternateApiEnabled} onChange={setApiBaseUrlChoice} aria-label={apiBaseUrlField.label} /><small>{apiBaseUrlField.description}</small></label> : null}
                {apiBaseUrlField && apiBaseUrlChoice === customApiBaseUrlOption ? <label htmlFor={"plugin-config-" + plugin.id + "-custom-api-base-url"}>自定义 API 地址<input id={"plugin-config-" + plugin.id + "-custom-api-base-url"} type="url" value={customApiBaseUrl} disabled={!alternateApiEnabled} onChange={(event) => setCustomApiBaseUrl(event.target.value)} placeholder="https://example.com" autoComplete="url" /><small>只填写 TMDb API 的基础地址，不要附带查询参数。</small></label> : null}
              </>}
              <p>{configField?.description ?? "插件配置"} 当前：{availabilityLabel(plugin.configSource)}。</p>
              <div className="lux-admin-plugin-dialog-actions">
                <button className="lux-button lux-button-secondary" type="button" onClick={closeDialog}>取消</button>
                <button className="lux-button lux-button-primary" type="submit" disabled={save.isPending}><Save size={15} /> {save.isPending ? "保存中…" : "保存配置"}</button>
              </div>
              {save.error ? <span className="lux-error-copy" role="alert">{save.error.message}</span> : null}
            </form>
          </section>
        </div>
      ) : null}
      {uninstallDialogOpen ? (
        <div className="lux-admin-plugin-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setUninstallDialogOpen(false); }}>
          <section className="lux-admin-plugin-dialog lux-admin-plugin-uninstall-dialog" role="alertdialog" aria-modal="true" aria-labelledby={`plugin-uninstall-title-${plugin.id}`}>
            <div className="lux-admin-plugin-dialog-heading">
              <div><h2 id={`plugin-uninstall-title-${plugin.id}`}>卸载插件</h2><p className="lux-admin-plugin-dialog-copy">确定要卸载 {plugin.name}吗？这会停止插件并移除已安装的插件包。</p></div>
            </div>
            <div className="lux-admin-plugin-dialog-actions">
              <button ref={uninstallCancelRef} className="lux-button lux-button-secondary" type="button" onClick={() => setUninstallDialogOpen(false)}>取消</button>
              <button className="lux-button lux-button-primary lux-admin-plugin-uninstall-confirm" type="button" aria-label={`确认卸载 ${plugin.name}`} disabled={uninstalling} onClick={() => { setUninstallDialogOpen(false); onUninstall(); }}>{uninstalling ? "卸载中…" : "确认卸载"}</button>
            </div>
          </section>
        </div>
      ) : null}
    </article>
  );
}

export function pluginCategoryLabel(category: string) {
  const normalized = category.trim().toUpperCase();
  if (normalized === "SCRAPER") return "刮削器";
  if (normalized === "PLAYBACK") return "播放";
  if (normalized === "UTILITY") return "工具";
  return category || "未分类";
}

function availabilityLabel(source: AdminPlugin["configSource"]) {
  if (source === "CUSTOM") return "使用自定义 Key";
  if (source === "ENVIRONMENT") return "使用环境变量 Key";
  if (source === "READ_ACCESS_TOKEN") return "使用 Read Access Token";
  if (source === "PLUGIN_DEFAULT") return "使用插件默认凭据";
  if (source === "PLUGIN_CONFIG") return "使用插件配置";
  return "未配置凭据";
}

function AdminPluginsState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "插件库加载失败" : "正在加载插件库"}</h1><p>{label}</p></section>;
}
