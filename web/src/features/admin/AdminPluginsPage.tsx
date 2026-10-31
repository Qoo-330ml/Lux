import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, Download, Globe2, PackageOpen, RefreshCw, Save, Settings2, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminPlugin } from "../../lib/api/types";
import { LuxSelect } from "../../components/LuxSelect";
import { EmbyMigrationPluginConfig } from "./EmbyMigrationPluginConfig";
import "./plugin-library.css";
import { LuxButton, LuxIconButton, LuxSwitch, LuxTextField } from "../../components/ui";

const CONFIG_SELECT_FIELDS_RENDERED_EXPLICITLY = new Set([
  "apiBaseUrl",
  "apiBaseUrlPreset",
  "existingInfoPolicy",
  "fallbackLanguages",
  "libraryIds",
  "preferredLanguage",
]);
const LOGIN_BACKGROUND_IMAGE_TYPES = new Set(["image/jpeg", "image/png", "image/webp"]);
const MAX_PLUGIN_IMAGE_UPLOAD_BYTES = 5 * 1024 * 1024;
const MAX_PLUGIN_STORE_SOURCES = 10;

function validPluginStoreSource(value: string): string | null {
  const source = value.trim();
  try {
    const url = new URL(source);
    if (url.protocol !== "https:" || url.username || url.password || url.hash) return null;
    return source;
  } catch {
    return null;
  }
}

export function AdminPluginsPage() {
  const queryClient = useQueryClient();
  const [mode, setMode] = useState<"store" | "installed">("store");
  const plugins = useQuery({ queryKey: queryKeys.adminPlugins, queryFn: () => api.adminPlugins() });
  const installedPlugins = useQuery({ queryKey: queryKeys.adminInstalledPlugins, queryFn: () => api.adminInstalledPlugins() });
  const store = useQuery({ queryKey: queryKeys.adminPluginStore, queryFn: () => api.adminPluginStore() });
  const [storeUrls, setStoreUrls] = useState<string[]>([]);
  const [storeUrlDraft, setStoreUrlDraft] = useState("");
  const [storeUrlError, setStoreUrlError] = useState("");
  const [storeDialogOpen, setStoreDialogOpen] = useState(false);
  const storeDialogCloseRef = useRef<HTMLButtonElement>(null);
  const closeStoreDialog = useCallback(() => setStoreDialogOpen(false), []);
  const updateStore = useMutation({
    mutationFn: (urls: string[]) => api.updateAdminPluginStore(urls),
    onSuccess: (data) => {
      setStoreUrls(data.urls);
      closeStoreDialog();
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPluginStore });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
    },
  });
  const openStoreDialog = useCallback(() => {
    setStoreUrls(store.data?.urls ?? []);
    setStoreUrlDraft("");
    setStoreUrlError("");
    updateStore.reset();
    setStoreDialogOpen(true);
  }, [store.data?.urls, updateStore]);
  const addStoreUrl = useCallback(() => {
    const source = validPluginStoreSource(storeUrlDraft);
    if (!source) {
      setStoreUrlError("请输入以 https:// 开头的有效地址，且不要包含账号、密码或片段。");
      return;
    }
    if (!storeUrls.includes(source) && storeUrls.length >= MAX_PLUGIN_STORE_SOURCES) {
      setStoreUrlError(`最多可保存 ${MAX_PLUGIN_STORE_SOURCES} 个插件商店地址。`);
      return;
    }
    setStoreUrls((current) => current.includes(source) ? current : [...current, source]);
    setStoreUrlDraft("");
    setStoreUrlError("");
    updateStore.reset();
  }, [storeUrlDraft, storeUrls, updateStore]);
  const submitStoreSources = useCallback(() => {
    let sources = storeUrls;
    const draft = storeUrlDraft.trim();
    if (draft) {
      const source = validPluginStoreSource(draft);
      if (!source) {
        setStoreUrlError("请输入以 https:// 开头的有效地址，且不要包含账号、密码或片段。");
        return;
      }
      if (!sources.includes(source)) {
        if (sources.length >= MAX_PLUGIN_STORE_SOURCES) {
          setStoreUrlError(`最多可保存 ${MAX_PLUGIN_STORE_SOURCES} 个插件商店地址。`);
          return;
        }
        sources = [...sources, source];
        setStoreUrls(sources);
        setStoreUrlDraft("");
      }
    }
    updateStore.mutate(sources);
  }, [storeUrlDraft, storeUrls, updateStore]);
  const install = useMutation({
    mutationFn: (pluginId: string) => api.installAdminPlugin(pluginId),
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
  const update = useMutation({
    mutationFn: (pluginId: string) => api.updateAdminPlugin(pluginId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });
  const checkUpdates = useMutation({
    mutationFn: async () => {
      await Promise.all([plugins.refetch(), installedPlugins.refetch()]);
    },
  });

  useEffect(() => {
    if (!storeDialogOpen && store.data?.urls) setStoreUrls(store.data.urls);
  }, [store.data, storeDialogOpen]);

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

  const activePlugins = mode === "store" ? plugins : installedPlugins;
  if (activePlugins.error && !activePlugins.data) return <AdminPluginsState label={activePlugins.error.message || "插件列表加载失败"} error />;
  if (activePlugins.isPending) return <AdminPluginsState label={mode === "store" ? "正在读取插件商店…" : "正在读取已安装插件…"} />;

  const items = activePlugins.data?.plugins ?? [];
  const auxiliaryLoading = (mode === "store" ? installedPlugins.isPending : plugins.isPending) || store.isPending;
  const auxiliaryError = (mode === "store" ? installedPlugins.error : plugins.error) || store.error;
  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading">
        <div><h1>插件库</h1><p>从插件商店安装经过校验的插件，再为媒体库选择刮削器。</p></div>
        <div className="lux-admin-plugin-heading-actions">
          <LuxButton tone="secondary" size="compact" type="button" aria-label="检查插件更新" onClick={() => checkUpdates.mutate()} disabled={checkUpdates.isPending}><RefreshCw size={15} /> {checkUpdates.isPending ? "检查中…" : "检查更新"}</LuxButton>
          <LuxButton tone="secondary" size="compact" className="lux-admin-plugin-store-trigger" type="button" aria-label="设置插件商店来源" aria-haspopup="dialog" aria-expanded={storeDialogOpen} onClick={openStoreDialog}><Globe2 size={15} /> 插件商店来源</LuxButton>
        </div>
      </header>
      {auxiliaryLoading ? <p className="lux-admin-muted" role="status">当前列表已加载，正在读取其他插件数据…</p> : null}
      {auxiliaryError ? <p className="lux-error-copy" role="alert">部分插件数据加载失败：{auxiliaryError.message}</p> : null}
      {storeDialogOpen ? (
        <div className="lux-admin-plugin-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) closeStoreDialog(); }}>
          <section className="lux-admin-plugin-dialog lux-admin-plugin-store-dialog" role="dialog" aria-modal="true" aria-labelledby="plugin-store-source-dialog-title">
            <div className="lux-admin-plugin-dialog-heading">
              <div><h2 id="plugin-store-source-dialog-title">插件商店来源</h2><p className="lux-admin-plugin-dialog-copy">可添加多个插件目录地址；GitHub 仓库地址会自动读取其 main/index.json。</p></div>
              <LuxIconButton className="lux-admin-plugin-dialog-close" ref={storeDialogCloseRef} type="button" aria-label="关闭插件商店来源设置" onClick={closeStoreDialog}><X size={17} /></LuxIconButton>
            </div>
            <form className="lux-admin-plugin-dialog-form" onSubmit={(event) => { event.preventDefault(); submitStoreSources(); }}>
              {store.isPending ? <p className="lux-admin-muted" role="status">正在读取插件商店来源…</p> : null}
              {store.error ? <p className="lux-error-copy" role="alert">插件商店来源加载失败：{store.error.message}</p> : null}
              <div className="lux-admin-plugin-store-source-field">
                <label htmlFor="lux-plugin-store-url">目录地址</label>
                <div className="lux-admin-plugin-store-source-input">
                  <LuxTextField
                    id="lux-plugin-store-url"
                    type="url"
                    value={storeUrlDraft}
                    onChange={(event) => { setStoreUrlDraft(event.target.value); setStoreUrlError(""); updateStore.reset(); }}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") {
                        event.preventDefault();
                        addStoreUrl();
                      }
                    }}
                    placeholder={store.data?.defaultUrl}
                  />
                  <LuxButton tone="secondary" type="button" aria-label="添加插件商店来源" onClick={addStoreUrl}>添加</LuxButton>
                </div>
                <div className="lux-admin-plugin-store-source-tags" aria-live="polite" aria-label="已添加的插件商店地址">
                  {storeUrls.length === 0 ? <span className="lux-admin-plugin-store-source-placeholder">尚未添加来源，保存后将使用默认来源。</span> : storeUrls.map((url, index) => (
                    <span className="lux-admin-plugin-store-source-tag" key={url} title={url}>
                      <span>{url}</span>
                      <button type="button" aria-label={`移除插件商店来源 ${url}`} onClick={() => {
                        setStoreUrlError("");
                        updateStore.reset();
                        setStoreUrls((current) => current.filter((_, itemIndex) => itemIndex !== index));
                      }}>
                        <X size={12} aria-hidden="true" />
                      </button>
                    </span>
                  ))}
                </div>
                <small>{storeUrls.length}/{MAX_PLUGIN_STORE_SOURCES} 个来源。回车可直接添加；同一插件 ID 以更高版本为准。</small>
              </div>
              {storeUrlError ? <p className="lux-error-copy" role="alert">{storeUrlError}</p> : null}
              <div className="lux-admin-plugin-dialog-actions">
                <LuxButton tone="secondary" type="button" onClick={closeStoreDialog}>取消</LuxButton>
                <LuxButton tone="primary" type="submit" disabled={updateStore.isPending}>{updateStore.isPending ? "保存中…" : "保存来源"}</LuxButton>
              </div>
              {updateStore.error ? <span className="lux-error-copy" role="alert">{updateStore.error.message}</span> : null}
            </form>
          </section>
        </div>
      ) : null}
      <nav className="lux-admin-plugin-tabs" aria-label="插件库视图">
        <button className={mode === "store" ? "is-active" : ""} type="button" aria-pressed={mode === "store"} onClick={() => setMode("store")}>插件商店<span>{plugins.data?.total ?? plugins.data?.plugins?.length ?? 0}</span></button>
        <button className={mode === "installed" ? "is-active" : ""} type="button" aria-pressed={mode === "installed"} onClick={() => setMode("installed")}>已安装管理<span>{installedPlugins.data?.total ?? installedPlugins.data?.plugins?.length ?? 0}</span></button>
      </nav>
      <section className="lux-admin-plugin-grid" aria-label="可用插件">
        {items.length === 0 ? <div className="lux-admin-empty"><PackageOpen size={24} /><h2>{mode === "store" ? "暂无可用插件" : "还没有已安装插件"}</h2><p>{mode === "store" ? "插件目录为空，请稍后重试。" : "从插件商店安装插件后，会在这里统一配置和管理。"}</p></div> : items.map((plugin) => <PluginCard key={plugin.id} plugin={plugin} installing={install.isPending && install.variables === plugin.id} installedManagement={mode === "installed"} toggling={toggleEnabled.isPending && toggleEnabled.variables?.pluginId === plugin.id} uninstalling={uninstall.isPending && uninstall.variables === plugin.id} updating={update.isPending && update.variables === plugin.id} onInstall={() => install.mutate(plugin.id)} onToggleEnabled={(enabled) => toggleEnabled.mutate({ pluginId: plugin.id, enabled })} onUninstall={() => uninstall.mutate(plugin.id)} onUpdate={() => update.mutate(plugin.id)} />)}
      </section>
      {install.error || uninstall.error || toggleEnabled.error || update.error || checkUpdates.error ? <p className="lux-error-copy" role="alert">{install.error?.message || uninstall.error?.message || toggleEnabled.error?.message || update.error?.message || checkUpdates.error?.message}</p> : null}
    </div>
  );
}

function PluginCard({ plugin, installing, installedManagement, toggling, uninstalling, updating, onInstall, onToggleEnabled, onUninstall, onUpdate }: { plugin: AdminPlugin; installing: boolean; installedManagement: boolean; toggling: boolean; uninstalling: boolean; updating: boolean; onInstall: () => void; onToggleEnabled: (enabled: boolean) => void; onUninstall: () => void; onUpdate: () => void }) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [uninstallDialogOpen, setUninstallDialogOpen] = useState(false);
  const [apiKey, setApiKey] = useState("");
  const [apiKeyDirty, setApiKeyDirty] = useState(false);
  const [additionalSelectValues, setAdditionalSelectValues] = useState<Record<string, string | string[]>>({});
  const [additionalToggleValues, setAdditionalToggleValues] = useState<Record<string, boolean>>({});
  const [uploadedImageFields, setUploadedImageFields] = useState<Record<string, boolean>>({});
  const [imageUploadErrors, setImageUploadErrors] = useState<Record<string, string>>({});
  const [danmakuProviderBaseUrl, setDanmakuProviderBaseUrl] = useState("");
  const [danmakuProviderBaseUrlDirty, setDanmakuProviderBaseUrlDirty] = useState(false);
  const [preferredLanguage, setPreferredLanguage] = useState("zh-CN");
  const [languageFallbackEnabled, setLanguageFallbackEnabled] = useState(false);
  const [titleAliasReplacementEnabled, setTitleAliasReplacementEnabled] = useState(false);
  const [originalLanguageEnabled, setOriginalLanguageEnabled] = useState(false);
  const [fallbackLanguages, setFallbackLanguages] = useState<string[]>(["zh-SG", "zh-HK", "zh-TW"]);
  const [alternateApiEnabled, setAlternateApiEnabled] = useState(false);
  const [apiBaseUrlChoice, setApiBaseUrlChoice] = useState("official");
  const [customApiBaseUrl, setCustomApiBaseUrl] = useState("");
  const [libraryIds, setLibraryIds] = useState<string[]>([]);
  const [matchOriginalFilename, setMatchOriginalFilename] = useState(true);
  const [matchSimplifiedTraditionalTitles, setMatchSimplifiedTraditionalTitles] = useState(true);
  const [matchEnglishTitle, setMatchEnglishTitle] = useState(false);
  const [concurrency, setConcurrency] = useState(2);
  const [overwrite, setOverwrite] = useState(false);
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
  const isDanmaku = plugin.id === "org.lux.danmaku";
  const isMediaInfo = plugin.id === "org.lux.strm-media-info";
  const isMigration = plugin.id === "org.lux.emby-migration";
  const isLoginBackgroundProvider = plugin.capabilities?.includes("login_background.get") === true;
  const isChapterSource = plugin.capabilities?.some((capability) => capability === "chapters.detect" || capability === "chapters.lookup") === true;
  const configField = plugin.configFields.find((field) => field.key === "apiKey");
  const danmakuProviderField = plugin.configFields.find((field) => field.key === "providerBaseUrl");
  const preferredLanguageField = plugin.configFields.find((field) => field.key === "preferredLanguage");
  const fallbackEnabledField = plugin.configFields.find((field) => field.key === "languageFallbackEnabled");
  const titleAliasReplacementField = plugin.configFields.find((field) => field.key === "titleAliasReplacementEnabled");
  const originalLanguageField = plugin.configFields.find((field) => field.key === "originalLanguageEnabled");
  const fallbackLanguagesField = plugin.configFields.find((field) => field.key === "fallbackLanguages");
  const alternateApiField = plugin.configFields.find((field) => field.key === "alternateApiEnabled");
  const apiBaseUrlField = plugin.configFields.find((field) => field.key === "apiBaseUrl");
  const apiBaseUrlPresetField = plugin.configFields.find((field) => field.key === "apiBaseUrlPreset")
    ?? (apiBaseUrlField?.type === "select" ? apiBaseUrlField : undefined);
  const customApiBaseUrlField = plugin.configFields.find((field) => field.key === "customApiBaseUrl")
    ?? (apiBaseUrlField?.type === "text" ? apiBaseUrlField : undefined);
  const libraryIdsField = plugin.configFields.find((field) => field.key === "libraryIds");
  const concurrencyField = plugin.configFields.find((field) => field.key === "concurrency");
  const overwriteField = plugin.configFields.find((field) => field.key === "overwrite");
  const introWindowField = plugin.configFields.find((field) => field.key === "introWindowSeconds");
  const creditsWindowField = plugin.configFields.find((field) => field.key === "creditsWindowSeconds");
  const matchThresholdField = plugin.configFields.find((field) => field.key === "matchThreshold");
  const existingInfoPolicyField = plugin.configFields.find((field) => field.key === "existingInfoPolicy");
  const mediaInfoEnabledField = plugin.configFields.find((field) => field.key === "mediaInfoEnabled");
  const thumbnailEnabledField = plugin.configFields.find((field) => field.key === "thumbnailEnabled");
  const thumbnailPositionPercentField = plugin.configFields.find((field) => field.key === "thumbnailPositionPercent");
  const writeSidecarsField = plugin.configFields.find((field) => field.key === "writeSidecars");
  const scheduleField = plugin.configFields.find((field) => field.key === "schedule");
  const additionalSelectFields = plugin.configFields.filter((field) =>
    field.type === "select" && !CONFIG_SELECT_FIELDS_RENDERED_EXPLICITLY.has(field.key));
  const additionalToggleFields = plugin.configFields.filter((field) => field.type === "toggle");
  const imageFields = plugin.configFields.filter((field) => field.type === "image");
  const additionalSelectConfig = additionalSelectFields.reduce<Record<string, string | string[]>>((config, field) => {
    const value = additionalSelectValues[field.key];
    if (Array.isArray(value) || (typeof value === "string" && value !== "")) {
      config[field.key] = value;
    }
    return config;
  }, {});
  const additionalToggleConfig = additionalToggleFields.reduce<Record<string, boolean>>((config, field) => {
    const value = additionalToggleValues[field.key];
    if (typeof value === "boolean") config[field.key] = value;
    return config;
  }, {});
  const requiredSelectMissing = additionalSelectFields.some((field) => {
    const value = additionalSelectValues[field.key];
    return field.required && (Array.isArray(value) ? value.length === 0 : !value);
  });
  const customApiBaseUrlOption = apiBaseUrlPresetField?.options?.find((option) => option.label === "自定义")?.value ?? "custom";
  const canConfigure = plugin.installed && plugin.configurable && plugin.configFields.length > 0;
  const toggleBlockedByProvider = plugin.unavailableReason === "OTHER_IP_LOCATION_PLUGIN_INSTALLED";
  const closeDialog = useCallback(() => setOpen(false), []);
  const save = useMutation({
    mutationFn: () => isDanmaku
      ? api.updateAdminPluginConfig(plugin.id, {
          ...(danmakuProviderBaseUrlDirty ? { providerBaseUrl: danmakuProviderBaseUrl.trim() } : {}),
          libraryIds,
          matchOriginalFilename,
          matchSimplifiedTraditionalTitles,
          matchEnglishTitle,
          concurrency,
          overwrite,
          ...additionalSelectConfig,
        })
      : isMediaInfo
        ? api.updateAdminPluginConfig(plugin.id, {
          libraryIds,
          concurrency,
          existingInfoPolicy,
          ...(mediaInfoEnabledField ? { mediaInfoEnabled } : {}),
          ...(thumbnailEnabledField ? { thumbnailEnabled } : {}),
          ...(thumbnailPositionPercentField ? { thumbnailPositionPercent } : {}),
          writeSidecars,
          ...(scheduleField ? { schedule: schedule.trim() } : {}),
          ...additionalSelectConfig,
          })
        : isChapterSource
          ? api.updateAdminPluginConfig(plugin.id, {
            concurrency,
            ...(introWindowField ? { introWindowSeconds } : {}),
            ...(creditsWindowField ? { creditsWindowSeconds } : {}),
            ...(matchThresholdField ? { matchThreshold } : {}),
            ...(scheduleField ? { schedule: schedule.trim() } : {}),
            ...additionalSelectConfig,
            })
          : isLoginBackgroundProvider
            ? api.updateAdminPluginConfig(plugin.id, {
              ...additionalSelectConfig,
              ...additionalToggleConfig,
            })
          : api.updateAdminPluginConfig(plugin.id, {
          ...(apiKeyDirty ? { apiKey } : {}),
          preferredLanguage,
          languageFallbackEnabled,
          titleAliasReplacementEnabled,
          ...(originalLanguageField ? { originalLanguageEnabled } : {}),
          fallbackLanguages,
          alternateApiEnabled,
          ...(apiBaseUrlPresetField?.key === "apiBaseUrlPreset"
            ? {
              apiBaseUrlPreset: apiBaseUrlChoice,
              apiBaseUrl: apiBaseUrlChoice === customApiBaseUrlOption
                ? customApiBaseUrl.trim()
                : apiBaseUrlPresetField.options?.find((option) => option.value === apiBaseUrlChoice)?.label ?? "",
            }
            : apiBaseUrlField
              ? {
                apiBaseUrl: apiBaseUrlField.type === "select"
                  ? apiBaseUrlChoice
                  : customApiBaseUrl.trim(),
                ...(customApiBaseUrlField?.key === "customApiBaseUrl"
                  ? { customApiBaseUrl: customApiBaseUrl.trim() }
                  : {}),
              }
              : {}),
          ...additionalSelectConfig,
        }),
    onSuccess: () => {
      setApiKey("");
      setDanmakuProviderBaseUrl("");
      setDanmakuProviderBaseUrlDirty(false);
      closeDialog();
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });
  const uploadImage = useMutation({
    mutationFn: ({ fieldKey, file }: { fieldKey: string; file: File }) =>
      api.uploadAdminPluginConfigImage(plugin.id, fieldKey, file),
    onSuccess: (_result, { fieldKey }) => {
      setUploadedImageFields((current) => ({ ...current, [fieldKey]: true }));
      setImageUploadErrors((current) => ({ ...current, [fieldKey]: "" }));
    },
    onError: (error, { fieldKey }) => {
      setImageUploadErrors((current) => ({ ...current, [fieldKey]: error.message }));
    },
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
      : ["zh-TW"];
    const configuredApiBaseUrl = typeof values.apiBaseUrl === "string"
      ? values.apiBaseUrl
      : typeof apiBaseUrlField?.defaultValue === "string"
        ? apiBaseUrlField.defaultValue
        : apiBaseUrlPresetField?.options?.[0]?.label ?? "https://api.themoviedb.org";
    const configuredApiBaseUrlPreset = typeof values.apiBaseUrlPreset === "string"
      ? values.apiBaseUrlPreset
      : undefined;
    const selectedApiOption = apiBaseUrlPresetField?.options?.find(
      (option) => option.value !== customApiBaseUrlOption
        && (option.value === configuredApiBaseUrlPreset
          || option.value === configuredApiBaseUrl
          || option.label === configuredApiBaseUrl),
    );
    setPreferredLanguage(preferred);
    setLanguageFallbackEnabled(values.languageFallbackEnabled === true);
    setTitleAliasReplacementEnabled(values.titleAliasReplacementEnabled === true);
    setOriginalLanguageEnabled(values.originalLanguageEnabled === true);
    setFallbackLanguages(fallback);
    setAlternateApiEnabled(values.alternateApiEnabled === true);
    setApiBaseUrlChoice(selectedApiOption?.value ?? customApiBaseUrlOption);
    setCustomApiBaseUrl(typeof values.customApiBaseUrl === "string"
      ? values.customApiBaseUrl
      : selectedApiOption ? "" : configuredApiBaseUrl);
    const configuredLibraryIds = Array.isArray(values.libraryIds)
      ? values.libraryIds.filter((value): value is string => typeof value === "string")
      : [];
    setLibraryIds(configuredLibraryIds);
    setMatchOriginalFilename(values.matchOriginalFilename !== false);
    setMatchSimplifiedTraditionalTitles(values.matchSimplifiedTraditionalTitles !== false);
    setMatchEnglishTitle(values.matchEnglishTitle === true);
    setConcurrency(typeof values.concurrency === "number" ? values.concurrency : Number(concurrencyField?.defaultValue ?? 2));
    setOverwrite(values.overwrite === true);
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
    setAdditionalSelectValues(Object.fromEntries(additionalSelectFields.map((field) => {
      const configuredValue = values[field.key];
      const defaultValue = field.defaultValue;
      const value = field.multiple
        ? Array.isArray(configuredValue)
          ? configuredValue.filter((item): item is string => typeof item === "string")
          : Array.isArray(defaultValue)
            ? defaultValue.filter((item): item is string => typeof item === "string")
            : []
        : typeof configuredValue === "string"
          ? configuredValue
          : typeof defaultValue === "string" ? defaultValue : "";
      return [field.key, value];
    })));
    setAdditionalToggleValues(Object.fromEntries(additionalToggleFields.map((field) => {
      const configuredValue = values[field.key];
      return [field.key, typeof configuredValue === "boolean"
        ? configuredValue
        : field.defaultValue === true];
    })));
    setApiKey("");
    setApiKeyDirty(false);
    setDanmakuProviderBaseUrl("");
    setDanmakuProviderBaseUrlDirty(false);
  }, [apiBaseUrlField?.defaultValue, apiBaseUrlPresetField?.options, concurrencyField?.defaultValue, creditsWindowField?.defaultValue, customApiBaseUrlOption, existingInfoPolicyField?.defaultValue, introWindowField?.defaultValue, matchThresholdField?.defaultValue, open, originalLanguageField?.defaultValue, overwriteField?.defaultValue, plugin.configFields, plugin.configValues, preferredLanguageField?.options, scheduleField?.defaultValue, thumbnailPositionPercentField?.defaultValue, titleAliasReplacementField?.defaultValue]);

  return (
    <article className="lux-admin-plugin-card">
      <div className="lux-admin-plugin-icon" aria-hidden="true"><PackageOpen size={22} /></div>
      <div className="lux-admin-plugin-content">
        <div className="lux-admin-plugin-heading-line">
          <h2>{plugin.name}</h2>
          <div className="lux-admin-plugin-meta" aria-label="插件版本和分类">
            <span className="lux-admin-plugin-version">{plugin.version ? `v${plugin.version}` : "版本未知"}{plugin.latestVersion && plugin.latestVersion !== plugin.version ? ` · 最新 v${plugin.latestVersion}` : ""}</span>
            <span className="lux-admin-plugin-category">{pluginCategoryLabel(plugin.category)}</span>
          </div>
        </div>
        <p title={plugin.description}>{plugin.description}</p>
      </div>
      <div className="lux-admin-plugin-actions">
        {installedManagement && plugin.installed ? (
          <>
            {plugin.updateAvailable ? <LuxButton tone="secondary" className="lux-admin-plugin-update-button" type="button" aria-label={`更新插件 ${plugin.name}`} disabled={updating || uninstalling} onClick={onUpdate}><RefreshCw size={14} /> {updating ? "更新中…" : "更新插件"}</LuxButton> : null}
            <button className={`lux-admin-plugin-enable-switch${plugin.enabled ? " is-enabled" : ""}`} type="button" role="switch" aria-checked={plugin.enabled} aria-label={toggleBlockedByProvider ? `由其他插件停用 ${plugin.name}` : `${plugin.enabled ? "禁用" : "启用"} ${plugin.name}`} disabled={toggling || uninstalling || toggleBlockedByProvider} onClick={() => onToggleEnabled(!plugin.enabled)}>
              <span className="lux-admin-plugin-enable-switch-track" aria-hidden="true"><span /></span>
              <span>{plugin.enabled ? "已启用" : "已禁用"}</span>
            </button>
            <button className="lux-admin-plugin-uninstall-button" type="button" aria-label={`卸载 ${plugin.name}`} disabled={uninstalling} onClick={() => setUninstallDialogOpen(true)}><Trash2 size={14} /> 卸载</button>
          </>
        ) : plugin.installed ? (
          <>{plugin.updateAvailable ? <LuxButton tone="secondary" className="lux-admin-plugin-update-button" type="button" aria-label={`更新插件 ${plugin.name}`} disabled={updating} onClick={onUpdate}><RefreshCw size={14} /> {updating ? "更新中…" : "更新插件"}</LuxButton> : <span className="lux-admin-plugin-install-status is-installed" role="status" aria-label="插件状态：已安装"><CheckCircle2 size={15} /> 已安装</span>}</>
        ) : (
          <button className="lux-admin-plugin-install-status is-install" type="button" aria-label={`安装 ${plugin.name}`} disabled={installing} onClick={onInstall}><Download size={15} /> {installing ? "安装中…" : "安装"}</button>
        )}
        {canConfigure ? <button className="lux-admin-plugin-config-button" type="button" aria-label={`配置 ${plugin.name}`} onClick={() => setOpen(true)}><Settings2 size={15} /> 配置</button> : null}
      </div>
      {open && canConfigure ? (
        <div className="lux-admin-plugin-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) closeDialog(); }}>
          <section className={`lux-admin-plugin-dialog${isMigration ? " lux-admin-plugin-migration-dialog" : ""}`} role="dialog" aria-modal="true" aria-labelledby={`plugin-config-title-${plugin.id}`}>
            <div className="lux-admin-plugin-dialog-heading">
              <div><h2 id={`plugin-config-title-${plugin.id}`}>{plugin.name}</h2></div>
              <LuxIconButton className="lux-admin-plugin-dialog-close" ref={closeRef} type="button" aria-label={`关闭 ${plugin.name}配置`} onClick={closeDialog}><X size={17} /></LuxIconButton>
            </div>
            {isMigration ? <EmbyMigrationPluginConfig plugin={plugin} /> : <form className="lux-admin-plugin-dialog-form" autoComplete="off" onSubmit={(event) => { event.preventDefault(); if (!requiredSelectMissing) save.mutate(); }}>
              {isDanmaku ? <>
                {danmakuProviderField ? <label htmlFor={"plugin-config-" + plugin.id + "-provider-base-url"}>{danmakuProviderField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-provider-base-url"} type="url" value={danmakuProviderBaseUrl} onChange={(event) => { setDanmakuProviderBaseUrl(event.target.value); setDanmakuProviderBaseUrlDirty(true); }} placeholder="留空保留已保存的地址" autoComplete="url" required={danmakuProviderField.required && !plugin.configured} /><small>{danmakuProviderField.description}</small></label> : null}
                {libraryIdsField ? <label htmlFor={"plugin-config-" + plugin.id + "-library-ids"}>{libraryIdsField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-library-ids"} multiple value={libraryIds} options={libraryIdsField.options ?? []} onChange={setLibraryIds} aria-label={libraryIdsField.label} /><small>{libraryIdsField.description}</small></label> : null}
                <LuxSwitch className="lux-admin-plugin-toggle" checked={matchOriginalFilename} onChange={(event) => setMatchOriginalFilename(event.target.checked)} label=<>使用原始文件名</> description=<>优先使用视频文件名请求上游匹配接口。</> />
                <LuxSwitch className="lux-admin-plugin-toggle" checked={matchSimplifiedTraditionalTitles} onChange={(event) => setMatchSimplifiedTraditionalTitles(event.target.checked)} label=<>尝试简繁标题</> description=<>没有匹配结果时，尝试已登记的本地标题候选。</> />
                <LuxSwitch className="lux-admin-plugin-toggle" checked={matchEnglishTitle} onChange={(event) => setMatchEnglishTitle(event.target.checked)} label=<>尝试英文标题</> description=<>没有匹配结果时，尝试已登记的英文或原始标题。</> />
                {concurrencyField ? <label htmlFor={"plugin-config-" + plugin.id + "-concurrency"}>{concurrencyField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-concurrency"} type="number" min={concurrencyField.minimum ?? 0} max={concurrencyField.maximum ?? 64} step={1} value={concurrency} onChange={(event) => setConcurrency(Number(event.target.value))} /><small>{concurrencyField.description}</small></label> : null}
                {overwriteField ? <LuxSwitch className="lux-admin-plugin-toggle" checked={overwrite} onChange={(event) => setOverwrite(event.target.checked)} label=<>{overwriteField.label}</> description=<>{overwriteField.description}</> /> : null}
              </> : isMediaInfo ? <>
                {libraryIdsField ? <label htmlFor={"plugin-config-" + plugin.id + "-library-ids"}>{libraryIdsField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-library-ids"} multiple value={libraryIds} options={libraryIdsField.options ?? []} onChange={setLibraryIds} aria-label={libraryIdsField.label} /><small>{libraryIdsField.description}</small></label> : null}
                {concurrencyField ? <label htmlFor={"plugin-config-" + plugin.id + "-concurrency"}>{concurrencyField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-concurrency"} type="number" min={concurrencyField.minimum ?? 1} max={concurrencyField.maximum ?? 64} value={concurrency} onChange={(event) => setConcurrency(Number(event.target.value))} /><small>{concurrencyField.description}</small></label> : null}
                {introWindowField ? <label htmlFor={"plugin-config-" + plugin.id + "-intro-window"}>{introWindowField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-intro-window"} type="number" min={introWindowField.minimum ?? 15} max={introWindowField.maximum ?? 300} value={introWindowSeconds} onChange={(event) => setIntroWindowSeconds(Number(event.target.value))} /><small>{introWindowField.description}</small></label> : null}
                {creditsWindowField ? <label htmlFor={"plugin-config-" + plugin.id + "-credits-window"}>{creditsWindowField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-credits-window"} type="number" min={creditsWindowField.minimum ?? 15} max={creditsWindowField.maximum ?? 600} value={creditsWindowSeconds} onChange={(event) => setCreditsWindowSeconds(Number(event.target.value))} /><small>{creditsWindowField.description}</small></label> : null}
                {matchThresholdField ? <label htmlFor={"plugin-config-" + plugin.id + "-match-threshold"}>{matchThresholdField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-match-threshold"} type="number" min={matchThresholdField.minimum ?? 1} max={matchThresholdField.maximum ?? 100} value={matchThreshold} onChange={(event) => setMatchThreshold(Number(event.target.value))} /><small>{matchThresholdField.description}</small></label> : null}
                {existingInfoPolicyField ? <label htmlFor={"plugin-config-" + plugin.id + "-existing-info-policy"}>{existingInfoPolicyField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-existing-info-policy"} value={existingInfoPolicy} options={existingInfoPolicyField.options ?? []} onChange={setExistingInfoPolicy} aria-label={existingInfoPolicyField.label} /><small>{existingInfoPolicyField.description}</small></label> : null}
                {mediaInfoEnabledField ? <LuxSwitch className="lux-admin-plugin-toggle" checked={mediaInfoEnabled} onChange={(event) => setMediaInfoEnabled(event.target.checked)} label=<>{mediaInfoEnabledField.label}</> description=<>{mediaInfoEnabledField.description}</> /> : null}
                {thumbnailEnabledField ? <LuxSwitch className="lux-admin-plugin-toggle" checked={thumbnailEnabled} onChange={(event) => setThumbnailEnabled(event.target.checked)} label=<>{thumbnailEnabledField.label}</> description=<>{thumbnailEnabledField.description}</> /> : null}
                {thumbnailPositionPercentField ? <label htmlFor={"plugin-config-" + plugin.id + "-thumbnail-position-percent"}>{thumbnailPositionPercentField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-thumbnail-position-percent"} type="number" required={thumbnailPositionPercentField.required} min={thumbnailPositionPercentField.minimum ?? 1} max={thumbnailPositionPercentField.maximum ?? 99} value={thumbnailPositionPercent} onChange={(event) => setThumbnailPositionPercent(Number(event.target.value))} /><small>{thumbnailPositionPercentField.description}</small></label> : null}
                {writeSidecarsField ? <LuxSwitch className="lux-admin-plugin-toggle" checked={writeSidecars} onChange={(event) => setWriteSidecars(event.target.checked)} label=<>{writeSidecarsField.label}</> description=<>{writeSidecarsField.description}</> /> : null}
                {scheduleField ? <label htmlFor={"plugin-config-" + plugin.id + "-schedule"}>{scheduleField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-schedule"} type="text" required={scheduleField.required} value={schedule} onChange={(event) => setSchedule(event.target.value)} placeholder="0 3 * * *" /><small>{scheduleField.description}</small></label> : null}
              </> : isChapterSource ? <>
                {concurrencyField ? <label htmlFor={"plugin-config-" + plugin.id + "-concurrency"}>{concurrencyField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-concurrency"} type="number" min={concurrencyField.minimum ?? 1} max={concurrencyField.maximum ?? 64} value={concurrency} onChange={(event) => setConcurrency(Number(event.target.value))} /><small>{concurrencyField.description}</small></label> : null}
                {introWindowField ? <label htmlFor={"plugin-config-" + plugin.id + "-intro-window"}>{introWindowField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-intro-window"} type="number" min={introWindowField.minimum ?? 15} max={introWindowField.maximum ?? 300} value={introWindowSeconds} onChange={(event) => setIntroWindowSeconds(Number(event.target.value))} /><small>{introWindowField.description}</small></label> : null}
                {creditsWindowField ? <label htmlFor={"plugin-config-" + plugin.id + "-credits-window"}>{creditsWindowField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-credits-window"} type="number" min={creditsWindowField.minimum ?? 15} max={creditsWindowField.maximum ?? 600} value={creditsWindowSeconds} onChange={(event) => setCreditsWindowSeconds(Number(event.target.value))} /><small>{creditsWindowField.description}</small></label> : null}
                {matchThresholdField ? <label htmlFor={"plugin-config-" + plugin.id + "-match-threshold"}>{matchThresholdField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-match-threshold"} type="number" min={matchThresholdField.minimum ?? 1} max={matchThresholdField.maximum ?? 100} value={matchThreshold} onChange={(event) => setMatchThreshold(Number(event.target.value))} /><small>{matchThresholdField.description}</small></label> : null}
                {scheduleField ? <label htmlFor={"plugin-config-" + plugin.id + "-schedule"}>{scheduleField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-schedule"} type="text" required={scheduleField.required} value={schedule} onChange={(event) => setSchedule(event.target.value)} placeholder="0 3 * * *" /><small>{scheduleField.description}</small></label> : null}
              </> : <>
                {configField ? <label htmlFor={"plugin-config-" + plugin.id + "-api-key"}>{configField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-api-key"} type="password" value={apiKey} onChange={(event) => { setApiKey(event.target.value); setApiKeyDirty(true); }} placeholder="留空使用插件默认凭据" autoComplete="new-password" /></label> : null}
                {preferredLanguageField ? <label htmlFor={"plugin-config-" + plugin.id + "-preferred-language"}>{preferredLanguageField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-preferred-language"} value={preferredLanguage} options={preferredLanguageField.options ?? []} onChange={setPreferredLanguage} aria-label={preferredLanguageField.label} /></label> : null}
                {titleAliasReplacementField ? <LuxSwitch className="lux-admin-plugin-toggle" checked={titleAliasReplacementEnabled} onChange={(event) => setTitleAliasReplacementEnabled(event.target.checked)} label=<>{titleAliasReplacementField.label}</> description=<>{titleAliasReplacementField.description}</> /> : null}
                {originalLanguageField ? <LuxSwitch className="lux-admin-plugin-toggle" checked={originalLanguageEnabled} onChange={(event) => setOriginalLanguageEnabled(event.target.checked)} label=<>{originalLanguageField.label}</> description=<>{originalLanguageField.description}</> /> : null}
                {fallbackEnabledField ? <LuxSwitch className="lux-admin-plugin-toggle" checked={languageFallbackEnabled} onChange={(event) => setLanguageFallbackEnabled(event.target.checked)} label=<>{fallbackEnabledField.label}</> description=<>{fallbackEnabledField.description}</> /> : null}
                {fallbackLanguagesField ? <label htmlFor={"plugin-config-" + plugin.id + "-fallback-languages"}>{fallbackLanguagesField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-fallback-languages"} multiple value={fallbackLanguages} options={fallbackLanguagesField.options ?? []} onChange={setFallbackLanguages} aria-label={fallbackLanguagesField.label} /><small>{fallbackLanguagesField.description}</small></label> : null}
                {alternateApiField ? <LuxSwitch className="lux-admin-plugin-toggle" checked={alternateApiEnabled} onChange={(event) => setAlternateApiEnabled(event.target.checked)} label=<>{alternateApiField.label}</> description=<>{alternateApiField.description}</> /> : null}
                {apiBaseUrlPresetField ? <label htmlFor={"plugin-config-" + plugin.id + "-api-base-url"}>{apiBaseUrlPresetField.label}<LuxSelect id={"plugin-config-" + plugin.id + "-api-base-url"} value={apiBaseUrlChoice} options={apiBaseUrlPresetField.options ?? []} disabled={!alternateApiEnabled} onChange={setApiBaseUrlChoice} aria-label={apiBaseUrlPresetField.label} /><small>{apiBaseUrlPresetField.description}</small></label> : null}
                {customApiBaseUrlField && apiBaseUrlChoice === customApiBaseUrlOption ? <label htmlFor={"plugin-config-" + plugin.id + "-custom-api-base-url"}>{customApiBaseUrlField.label}<LuxTextField id={"plugin-config-" + plugin.id + "-custom-api-base-url"} type="url" value={customApiBaseUrl} disabled={!alternateApiEnabled} onChange={(event) => setCustomApiBaseUrl(event.target.value)} placeholder="https://example.com" autoComplete="url" /><small>{customApiBaseUrlField.description}</small></label> : null}
              </>}
              {isLoginBackgroundProvider ? (
                <>
                  {additionalToggleFields.map((field) => (
                    <LuxSwitch key={field.key} className="lux-admin-plugin-toggle" checked={additionalToggleValues[field.key] === true} onChange={(event) => setAdditionalToggleValues((current) => ({ ...current, [field.key]: event.target.checked, }))} label=<><strong>{field.label}</strong>
                        {field.description ? <small>{field.description}</small> : null}</> />
                  ))}
                  {imageFields.map((field) => {
                    const savedAsset = plugin.configValues?.[field.key];
                    const hasUploadedImage = uploadedImageFields[field.key] === true
                      || (typeof savedAsset === "string" && /^sha256:[a-f0-9]{64}$/.test(savedAsset));
                    const inputId = `plugin-config-${plugin.id}-${field.key}`;
                    return (
                      <label className="lux-admin-plugin-image-field" htmlFor={inputId} key={field.key}>
                        <strong>{field.label}</strong>
                        <input
                          id={inputId}
                          type="file"
                          accept="image/jpeg,image/png,image/webp,.jpg,.jpeg,.png,.webp"
                          aria-label={`${field.label}（上传后立即替换旧图）`}
                          disabled={uploadImage.isPending}
                          onChange={(event) => {
                            const input = event.currentTarget;
                            const file = input.files?.[0];
                            input.value = "";
                            if (!file) return;
                            if (file.size > MAX_PLUGIN_IMAGE_UPLOAD_BYTES) {
                              setImageUploadErrors((current) => ({
                                ...current,
                                [field.key]: "图片不能超过 5 MiB；原有图片保持不变。",
                              }));
                              return;
                            }
                            if (file.type && !LOGIN_BACKGROUND_IMAGE_TYPES.has(file.type)) {
                              setImageUploadErrors((current) => ({
                                ...current,
                                [field.key]: "仅支持 JPEG、PNG 或 WebP；原有图片保持不变。",
                              }));
                              return;
                            }
                            setImageUploadErrors((current) => ({ ...current, [field.key]: "" }));
                            uploadImage.mutate({ fieldKey: field.key, file });
                          }}
                        />
                        {field.description ? <small>{field.description}</small> : null}
                        <small>上传成功会立即替换服务器托管图片；展示来源和许可确认仍需单独保存。</small>
                        <span className="lux-admin-plugin-image-status" role="status" aria-live="polite">
                          {uploadImage.isPending && uploadImage.variables?.fieldKey === field.key
                            ? "正在上传…"
                            : hasUploadedImage ? "已上传 1 张；选择新图可替换旧图。" : "尚未上传自定义图片。"}
                        </span>
                        {imageUploadErrors[field.key] ? <span className="lux-error-copy" role="alert">{imageUploadErrors[field.key]}</span> : null}
                      </label>
                    );
                  })}
                </>
              ) : null}
              {additionalSelectFields.map((field) => {
                const id = `plugin-config-${plugin.id}-${field.key}`;
                const value = additionalSelectValues[field.key];
                return <label key={field.key} htmlFor={id}>{field.label}{field.multiple ? <LuxSelect id={id} multiple value={Array.isArray(value) ? value : []} options={field.options ?? []} onChange={(selected) => setAdditionalSelectValues((current) => ({ ...current, [field.key]: selected }))} aria-label={field.label} /> : <LuxSelect id={id} value={typeof value === "string" ? value : ""} options={field.options ?? []} placeholder={field.required ? "请选择" : "可选"} onChange={(selected) => setAdditionalSelectValues((current) => ({ ...current, [field.key]: selected }))} aria-label={field.label} />}{field.description ? <small>{field.description}</small> : null}</label>;
              })}
              <p>{isLoginBackgroundProvider ? "展示来源与许可确认需要保存配置；自定义图片上传后会立即替换托管图片。" : `${danmakuProviderField?.description ?? configField?.description ?? "插件配置"} 当前：${availabilityLabel(plugin.configSource)}。`}</p>
              <div className="lux-admin-plugin-dialog-actions">
                <LuxButton tone="secondary" type="button" onClick={closeDialog}>取消</LuxButton>
                <LuxButton tone="primary" type="submit" disabled={save.isPending || uploadImage.isPending || requiredSelectMissing}><Save size={15} /> {save.isPending ? "保存中…" : "保存配置"}</LuxButton>
              </div>
              {save.error ? <span className="lux-error-copy" role="alert">{save.error.message}</span> : null}
            </form>}
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
              <LuxButton tone="secondary" ref={uninstallCancelRef} type="button" onClick={() => setUninstallDialogOpen(false)}>取消</LuxButton>
              <LuxButton tone="primary" className="lux-admin-plugin-uninstall-confirm" type="button" aria-label={`确认卸载 ${plugin.name}`} disabled={uninstalling} onClick={() => { setUninstallDialogOpen(false); onUninstall(); }}>{uninstalling ? "卸载中…" : "确认卸载"}</LuxButton>
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
