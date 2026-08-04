import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, Download, PackageOpen, RefreshCw, Save, Settings2, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminPlugin } from "../../lib/api/types";
import "./plugin-library.css";

export function AdminPluginsPage() {
  const queryClient = useQueryClient();
  const [mode, setMode] = useState<"store" | "installed">("store");
  const plugins = useQuery({ queryKey: queryKeys.adminPlugins, queryFn: () => api.adminPlugins() });
  const installedPlugins = useQuery({ queryKey: queryKeys.adminInstalledPlugins, queryFn: () => api.adminInstalledPlugins() });
  const install = useMutation({
    mutationFn: (pluginId: string) => api.installAdminPlugin(pluginId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });

  if (plugins.isPending || installedPlugins.isPending) return <AdminPluginsState label="正在读取插件库…" />;
  if (plugins.error || installedPlugins.error) return <AdminPluginsState label={plugins.error?.message || installedPlugins.error?.message || "插件库加载失败"} error />;

  const items = (mode === "store" ? plugins.data.plugins : installedPlugins.data.plugins) ?? [];
  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading">
        <div><span className="lux-eyebrow">PLUGIN LIBRARY</span><h1>插件库</h1><p>安装已内置并经过验证的元数据插件，再为媒体库选择刮削器。</p></div>
        <button className="lux-button lux-button-secondary lux-admin-refresh" type="button" onClick={() => { void plugins.refetch(); void installedPlugins.refetch(); }}><RefreshCw size={16} /> 刷新</button>
      </header>
      <nav className="lux-admin-plugin-tabs" aria-label="插件库视图">
        <button className={mode === "store" ? "is-active" : ""} type="button" aria-pressed={mode === "store"} onClick={() => setMode("store")}>插件商店<span>{plugins.data.total ?? plugins.data.plugins?.length ?? 0}</span></button>
        <button className={mode === "installed" ? "is-active" : ""} type="button" aria-pressed={mode === "installed"} onClick={() => setMode("installed")}>已安装管理<span>{installedPlugins.data.total ?? installedPlugins.data.plugins?.length ?? 0}</span></button>
      </nav>
      <section className="lux-admin-plugin-grid" aria-label="可用插件">
        {items.length === 0 ? <div className="lux-admin-empty"><PackageOpen size={24} /><h2>{mode === "store" ? "暂无可用插件" : "还没有已安装插件"}</h2><p>{mode === "store" ? "插件目录为空，请稍后重试。" : "从插件商店安装插件后，会在这里统一配置和管理。"}</p></div> : items.map((plugin) => <PluginCard key={plugin.id} plugin={plugin} installing={install.isPending && install.variables === plugin.id} onInstall={() => install.mutate(plugin.id)} />)}
      </section>
      {install.error ? <p className="lux-error-copy" role="alert">{install.error.message}</p> : null}
    </div>
  );
}

function PluginCard({ plugin, installing, onInstall }: { plugin: AdminPlugin; installing: boolean; onInstall: () => void }) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [apiKey, setApiKey] = useState("");
  const closeRef = useRef<HTMLButtonElement>(null);
  const configField = plugin.configFields.find((field) => field.key === "apiKey");
  const canConfigure = plugin.installed && plugin.configurable && configField !== undefined;
  const closeDialog = useCallback(() => setOpen(false), []);
  const save = useMutation({
    mutationFn: () => api.updateAdminPluginConfig(plugin.id, apiKey),
    onSuccess: () => {
      setApiKey("");
      closeDialog();
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
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
        {plugin.installed ? (
          <span className="lux-admin-plugin-install-status is-installed" role="status" aria-label="插件状态：已安装"><CheckCircle2 size={15} /> 已安装</span>
        ) : (
          <button className="lux-admin-plugin-install-status is-install" type="button" aria-label={`安装 ${plugin.name}`} disabled={installing} onClick={onInstall}><Download size={15} /> {installing ? "安装中…" : "安装"}</button>
        )}
        {canConfigure ? <button className="lux-admin-plugin-config-button" type="button" aria-label={`配置 ${plugin.name}`} onClick={() => setOpen(true)}><Settings2 size={15} /> 配置</button> : null}
      </div>
      {open && canConfigure && configField ? (
        <div className="lux-admin-plugin-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) closeDialog(); }}>
          <section className="lux-admin-plugin-dialog" role="dialog" aria-modal="true" aria-labelledby={`plugin-config-title-${plugin.id}`}>
            <div className="lux-admin-plugin-dialog-heading">
              <div><span className="lux-eyebrow">PLUGIN CONFIGURATION</span><h2 id={`plugin-config-title-${plugin.id}`}>{plugin.name}</h2></div>
              <button ref={closeRef} className="lux-icon-button lux-admin-plugin-dialog-close" type="button" aria-label={`关闭 ${plugin.name}配置`} onClick={closeDialog}><X size={17} /></button>
            </div>
            <form className="lux-admin-plugin-dialog-form" autoComplete="off" onSubmit={(event) => { event.preventDefault(); save.mutate(); }}>
              <label htmlFor={`plugin-config-${plugin.id}`}>{configField.label}<input id={`plugin-config-${plugin.id}`} type={configField.type} value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder="留空可恢复内置 Key" autoComplete="new-password" /></label>
              <p>{configField.description} 当前：{availabilityLabel(plugin.configSource)}。</p>
              <div className="lux-admin-plugin-dialog-actions">
                <button className="lux-button lux-button-secondary" type="button" onClick={closeDialog}>取消</button>
                <button className="lux-button lux-button-primary" type="submit" disabled={save.isPending}><Save size={15} /> {save.isPending ? "保存中…" : "保存配置"}</button>
              </div>
              {save.error ? <span className="lux-error-copy" role="alert">{save.error.message}</span> : null}
            </form>
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
  if (source === "BUILT_IN") return "使用内置 Key";
  return "未配置凭据";
}

function AdminPluginsState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><span className="lux-eyebrow">PLUGIN LIBRARY</span><h1>{error ? "插件库加载失败" : "正在加载插件库"}</h1><p>{label}</p></section>;
}
