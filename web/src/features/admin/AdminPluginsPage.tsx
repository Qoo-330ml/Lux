import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, Download, Globe2, PackageOpen, RefreshCw, Save, Settings2, Trash2, X } from "lucide-react";
import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminPlugin, AdminPluginConfigField } from "../../lib/api/types";
import { LuxSelect } from "../../components/LuxSelect";
import { EmbyMigrationPluginConfig } from "./EmbyMigrationPluginConfig";
import "./plugin-library.css";
import { LuxButton, LuxIconButton, LuxSwitch, LuxTextField, LuxTextarea } from "../../components/ui";

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

type PluginConfigPair = {
  select: AdminPluginConfigField;
  detail: AdminPluginConfigField;
  customOption: { value: string; label: string };
  gate: AdminPluginConfigField | null;
};

type PluginConfigValues = Record<string, unknown>;

function PluginCard({ plugin, installing, installedManagement, toggling, uninstalling, updating, onInstall, onToggleEnabled, onUninstall, onUpdate }: { plugin: AdminPlugin; installing: boolean; installedManagement: boolean; toggling: boolean; uninstalling: boolean; updating: boolean; onInstall: () => void; onToggleEnabled: (enabled: boolean) => void; onUninstall: () => void; onUpdate: () => void }) {
  const [open, setOpen] = useState(false);
  const [uninstallDialogOpen, setUninstallDialogOpen] = useState(false);
  const closeRef = useRef<HTMLButtonElement>(null);
  const uninstallCancelRef = useRef<HTMLButtonElement>(null);
  const isEmbyMigration = plugin.capabilities?.includes("migration.emby") === true;
  const canConfigure = plugin.installed && plugin.configurable && plugin.configFields.length > 0;
  const toggleBlockedByProvider = plugin.unavailableReason === "OTHER_IP_LOCATION_PLUGIN_INSTALLED";
  const closeDialog = useCallback(() => setOpen(false), []);

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
          <section className={`lux-admin-plugin-dialog${isEmbyMigration ? " lux-admin-plugin-migration-dialog" : ""}`} role="dialog" aria-modal="true" aria-labelledby={`plugin-config-title-${plugin.id}`}>
            <div className="lux-admin-plugin-dialog-heading">
              <div><h2 id={`plugin-config-title-${plugin.id}`}>{plugin.name}</h2></div>
              <LuxIconButton className="lux-admin-plugin-dialog-close" ref={closeRef} type="button" aria-label={`关闭 ${plugin.name}配置`} onClick={closeDialog}><X size={17} /></LuxIconButton>
            </div>
            {isEmbyMigration ? <EmbyMigrationPluginConfig plugin={plugin} /> : <PluginConfigForm key={plugin.id} plugin={plugin} onClose={closeDialog} />}
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

function PluginConfigForm({ plugin, onClose }: { plugin: AdminPlugin; onClose: () => void }) {
  const queryClient = useQueryClient();
  const pairs = configFieldPairs(plugin.configFields);
  const detailFieldKeys = new Set(Array.from(pairs.values(), (pair) => pair.detail.key));
  const [values, setValues] = useState<PluginConfigValues>(() => initialConfigValues(plugin, pairs));
  const [dirtyFields, setDirtyFields] = useState<Set<string>>(() => new Set());
  const [uploadedImageFields, setUploadedImageFields] = useState<Record<string, boolean>>({});
  const [imageUploadErrors, setImageUploadErrors] = useState<Record<string, string>>({});
  const requiredMissing = pluginConfigRequiredMissing(plugin, plugin.configFields, pairs, values);
  const updateValue = (key: string, value: unknown) => {
    setValues((current) => ({ ...current, [key]: value }));
  };
  const updateSecret = (key: string, value: unknown) => {
    updateValue(key, value);
    setDirtyFields((current) => {
      const next = new Set(current);
      next.add(key);
      return next;
    });
  };
  const updateField = (field: AdminPluginConfigField, value: unknown) => {
    if (isSensitiveConfigField(field)) updateSecret(field.key, value);
    else updateValue(field.key, value);
  };
  const save = useMutation({
    mutationFn: () => api.updateAdminPluginConfig(plugin.id, buildPluginConfigPayload(plugin.configFields, pairs, values, dirtyFields)),
    onSuccess: () => {
      setDirtyFields(new Set());
      onClose();
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

  const renderField = (field: AdminPluginConfigField) => {
    if (detailFieldKeys.has(field.key)) return null;
    const id = configFieldDomId(plugin.id, field);
    const pair = pairs.get(field.key);
    if (pair) {
      const selected = configStringValue(values, field.key);
      const gateEnabled = !pair.gate || values[pair.gate.key] === true;
      const detailId = configFieldDomId(plugin.id, pair.detail, true);
      return (
        <Fragment key={field.key}>
          <label htmlFor={id}>
            {field.label}
            <LuxSelect id={id} value={selected} options={field.options ?? []} placeholder={field.required ? "请选择" : "可选"} disabled={!gateEnabled} onChange={(next) => updateField(field, next)} aria-label={field.label} />
            {field.description ? <small>{field.description}</small> : null}
          </label>
          {selected === pair.customOption.value ? (
            <label htmlFor={detailId}>
              {pair.detail.label}
              <LuxTextField id={detailId} type="url" value={configStringValue(values, pair.detail.key)} disabled={!gateEnabled} onChange={(event) => updateField(pair.detail, event.target.value)} placeholder="https://example.com" autoComplete="url" />
              {pair.detail.description ? <small>{pair.detail.description}</small> : null}
            </label>
          ) : null}
        </Fragment>
      );
    }
    if (field.type === "toggle") {
      return <LuxSwitch key={field.key} id={id} className="lux-admin-plugin-toggle" checked={values[field.key] === true} onChange={(event) => updateField(field, event.target.checked)} label=<><strong>{field.label}</strong></> description={field.description ?? undefined} />;
    }
    if (field.type === "image") {
      const savedAsset = plugin.configValues?.[field.key];
      const hasUploadedImage = uploadedImageFields[field.key] === true || isUploadedPluginImage(savedAsset);
      return (
        <label className="lux-admin-plugin-image-field" htmlFor={id} key={field.key}>
          <strong>{field.label}</strong>
          <input
            id={id}
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
    }
    if (field.type === "select") {
      const value = values[field.key];
      return (
        <label key={field.key} htmlFor={id}>
          {field.label}
          {field.multiple ? (
            <LuxSelect id={id} multiple value={Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : []} options={field.options ?? []} onChange={(next) => updateField(field, next)} aria-label={field.label} />
          ) : (
            <LuxSelect id={id} value={typeof value === "string" ? value : ""} options={field.options ?? []} placeholder={field.required ? "请选择" : "可选"} onChange={(next) => updateField(field, next)} aria-label={field.label} />
          )}
          {field.description ? <small>{field.description}</small> : null}
        </label>
      );
    }
    if (field.type === "textarea") {
      return (
        <label key={field.key} htmlFor={id}>
          {field.label}
          <LuxTextarea id={id} value={configStringValue(values, field.key)} required={field.required} onChange={(event) => updateField(field, event.target.value)} />
          {field.description ? <small>{field.description}</small> : null}
        </label>
      );
    }
    if (field.type === "number") {
      const value = values[field.key];
      return (
        <label key={field.key} htmlFor={id}>
          {field.label}
          <LuxTextField id={id} type="number" min={field.minimum ?? undefined} max={field.maximum ?? undefined} step={1} value={typeof value === "number" && Number.isFinite(value) ? value : ""} required={field.required} onChange={(event) => updateField(field, Number(event.target.value))} />
          {field.description ? <small>{field.description}</small> : null}
        </label>
      );
    }
    const sensitive = isSensitiveConfigField(field);
    const urlField = looksLikeUrlField(field);
    return (
      <label key={field.key} htmlFor={id}>
        {field.label}
        <LuxTextField
          id={id}
          type={field.type === "password" ? "password" : urlField ? "url" : "text"}
          value={configStringValue(values, field.key)}
          onChange={(event) => updateField(field, event.target.value)}
          placeholder={sensitive ? (field.type === "password" ? "留空使用插件默认凭据" : "留空保留已保存的地址") : undefined}
          autoComplete={field.type === "password" ? "new-password" : urlField ? "url" : undefined}
          required={sensitive ? field.required && !plugin.configured : field.required}
        />
        {field.description ? <small>{field.description}</small> : null}
      </label>
    );
  };

  return (
    <form className="lux-admin-plugin-dialog-form" autoComplete="off" onSubmit={(event) => { event.preventDefault(); if (!requiredMissing) save.mutate(); }}>
      {plugin.configFields.map(renderField)}
      <p>{plugin.capabilities?.includes("login_background.get") ? "展示来源与许可确认需要保存配置；自定义图片上传后会立即替换托管图片。" : `${plugin.configFields[0]?.description ?? "插件配置"} 当前：${availabilityLabel(plugin.configSource)}。`}</p>
      <div className="lux-admin-plugin-dialog-actions">
        <LuxButton tone="secondary" type="button" onClick={onClose}>取消</LuxButton>
        <LuxButton tone="primary" type="submit" disabled={save.isPending || uploadImage.isPending || requiredMissing}><Save size={15} /> {save.isPending ? "保存中…" : "保存配置"}</LuxButton>
      </div>
      {save.error ? <span className="lux-error-copy" role="alert">{save.error.message}</span> : null}
    </form>
  );
}

function configFieldPairs(fields: AdminPluginConfigField[]) {
  const pairs = new Map<string, PluginConfigPair>();
  const claimed = new Set<string>();
  for (let index = 0; index < fields.length; index += 1) {
    const select = fields[index];
    if (!select) continue;
    const customOption = customSelectOption(select);
    if (!customOption) continue;
    const detail = fields[index + 1];
    if (!detail || claimed.has(detail.key) || !isEndpointDetailField(detail)) continue;
    const previous = fields[index - 1];
    const gate = previous?.type === "toggle" && !claimed.has(previous.key) ? previous : null;
    pairs.set(select.key, { select, detail, customOption, gate });
    claimed.add(detail.key);
    index += 1;
  }
  return pairs;
}

function customSelectOption(field: AdminPluginConfigField) {
  if (field.type !== "select" || field.multiple) return null;
  return field.options?.find((option) => {
    const label = String(option.label);
    return option.value === "custom" || label.trim() === "自定义" || label.trim().startsWith("自定义");
  }) ?? null;
}

function isEndpointDetailField(field: AdminPluginConfigField) {
  if (field.type !== "text") return false;
  const defaultValue = typeof field.defaultValue === "string" ? field.defaultValue : "";
  const text = `${field.label} ${field.description ?? ""}`;
  return /^https?:\/\//i.test(defaultValue) || /(?:地址|URL|url|endpoint|base\s*url)/i.test(text);
}

function initialConfigValues(plugin: AdminPlugin, pairs: Map<string, PluginConfigPair>): PluginConfigValues {
  const configured = plugin.configValues ?? {};
  const values: PluginConfigValues = {};
  for (const field of plugin.configFields) {
    if (field.type === "image") continue;
    if (isSensitiveConfigField(field)) {
      values[field.key] = "";
      continue;
    }
    const stored = configured[field.key];
    if (field.type === "toggle") {
      values[field.key] = typeof stored === "boolean" ? stored : field.defaultValue === true;
    } else if (field.type === "number") {
      values[field.key] = typeof stored === "number"
        ? stored
        : typeof field.defaultValue === "number"
          ? field.defaultValue
          : typeof field.minimum === "number" ? field.minimum : 0;
    } else if (field.type === "select") {
      if (field.multiple) {
        values[field.key] = Array.isArray(stored)
          ? stored.filter((item): item is string => typeof item === "string")
          : Array.isArray(field.defaultValue)
            ? field.defaultValue.filter((item): item is string => typeof item === "string")
            : [];
      } else {
        values[field.key] = typeof stored === "string"
          ? stored
          : typeof field.defaultValue === "string" ? field.defaultValue : "";
      }
    } else {
      values[field.key] = typeof stored === "string"
        ? stored
        : typeof field.defaultValue === "string" ? field.defaultValue : "";
    }
  }
  for (const pair of pairs.values()) {
    const rawSelect = values[pair.select.key];
    const rawDetail = values[pair.detail.key];
    let selected = typeof rawSelect === "string" ? rawSelect : "";
    const detailValue = typeof rawDetail === "string" ? rawDetail : "";
    if (!selected) {
      const matching = pair.select.options?.find((option) => option.value === detailValue || option.label === detailValue);
      selected = matching?.value
        ?? (detailValue
          ? pair.customOption.value
          : pair.select.options?.find((option) => option.value !== pair.customOption.value)?.value ?? "");
      values[pair.select.key] = selected;
    }
    if (selected !== pair.customOption.value) {
      // Only expose a custom detail input after choosing the custom option.
      values[pair.detail.key] = "";
    }
  }
  return values;
}

function buildPluginConfigPayload(fields: AdminPluginConfigField[], pairs: Map<string, PluginConfigPair>, values: PluginConfigValues, dirtyFields: Set<string>) {
  const detailFieldKeys = new Set(Array.from(pairs.values(), (pair) => pair.detail.key));
  const payload: Record<string, unknown> = {};
  for (const field of fields) {
    if (field.type === "image" || detailFieldKeys.has(field.key)) continue;
    const pair = pairs.get(field.key);
    if (pair) {
      const selected = configStringValue(values, field.key);
      payload[field.key] = selected;
      const selectedOption = field.options?.find((option) => option.value === selected);
      payload[pair.detail.key] = selected === pair.customOption.value
        ? String(values[pair.detail.key] ?? "").trim()
        : selectedOption?.label ?? "";
      continue;
    }
    if (isSensitiveConfigField(field)) {
      if (!dirtyFields.has(field.key)) continue;
      const raw = String(values[field.key] ?? "");
      payload[field.key] = field.type === "password" ? raw : raw.trim();
      continue;
    }
    const value = values[field.key];
    if (field.type === "toggle") {
      payload[field.key] = value === true;
    } else if (field.type === "number") {
      payload[field.key] = typeof value === "number" && Number.isFinite(value) ? value : 0;
    } else if (field.type === "select") {
      if (field.multiple) {
        payload[field.key] = Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
      } else {
        const selected = configStringValue(values, field.key);
        if (selected !== "") payload[field.key] = selected;
      }
    } else if (field.type === "text") {
      payload[field.key] = typeof value === "string" ? value.trim() : "";
    } else {
      payload[field.key] = typeof value === "string" ? value : "";
    }
  }
  return payload;
}

function pluginConfigRequiredMissing(plugin: AdminPlugin, fields: AdminPluginConfigField[], pairs: Map<string, PluginConfigPair>, values: PluginConfigValues) {
  const detailFieldKeys = new Set(Array.from(pairs.values(), (pair) => pair.detail.key));
  return fields.some((field) => {
    if (field.type === "image" || detailFieldKeys.has(field.key)) return false;
    if (isSensitiveConfigField(field)) {
      return field.required && !plugin.configured && String(values[field.key] ?? "").trim() === "";
    }
    const pair = pairs.get(field.key);
    if (pair) {
      const selected = configStringValue(values, field.key);
      if (field.required && selected === "") return true;
      return pair.detail.required && selected === pair.customOption.value && String(values[pair.detail.key] ?? "").trim() === "";
    }
    if (field.type === "toggle") return false;
    const value = values[field.key];
    if (field.type === "select") {
      return field.multiple
        ? field.required && (!Array.isArray(value) || value.length === 0)
        : field.required && (typeof value !== "string" || value === "");
    }
    if (field.type === "number") return field.required && !(typeof value === "number" && Number.isFinite(value));
    return field.required && String(value ?? "").trim() === "";
  });
}

function isSensitiveConfigField(field: AdminPluginConfigField) {
  return field.sensitive || field.type === "password";
}

function looksLikeUrlField(field: AdminPluginConfigField) {
  const defaultValue = typeof field.defaultValue === "string" ? field.defaultValue : "";
  return /^https?:\/\//i.test(defaultValue)
    || /(?:地址|URL|url|endpoint|base\s*url)/i.test(`${field.label} ${field.description ?? ""}`);
}

function configStringValue(values: PluginConfigValues, key: string) {
  const value = values[key];
  return typeof value === "string" ? value : "";
}

function configFieldDomId(pluginId: string, field: AdminPluginConfigField, customDetail = false) {
  const key = configFieldDomKey(field.key);
  return `plugin-config-${pluginId}-${customDetail && !key.startsWith("custom-") ? "custom-" : ""}${key}`;
}

function configFieldDomKey(key: string) {
  return key.replace(/([a-z0-9])([A-Z])/g, "$1-$2").replace(/([A-Z]+)([A-Z][a-z])/g, "$1-$2").toLowerCase();
}

function isUploadedPluginImage(value: unknown) {
  return typeof value === "string" && /^sha256:[a-f0-9]{64}$/.test(value);
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
