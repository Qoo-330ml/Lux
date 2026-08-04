import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, Download, PackageOpen, RefreshCw, Save, Settings2 } from "lucide-react";
import { useState } from "react";
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
        {items.length === 0 ? <div className="lux-admin-empty"><PackageOpen size={24} /><h2>{mode === "store" ? "暂无可用插件" : "还没有已安装插件"}</h2><p>{mode === "store" ? "插件目录为空，请稍后重试。" : "从插件商店安装插件后，会在这里统一配置和管理。"}</p></div> : items.map((plugin) => <PluginCard key={plugin.id} plugin={plugin} management={mode === "installed"} installing={install.isPending && install.variables === plugin.id} onInstall={() => install.mutate(plugin.id)} />)}
      </section>
      {install.error ? <p className="lux-error-copy" role="alert">{install.error.message}</p> : null}
    </div>
  );
}

function PluginCard({ plugin, management, installing, onInstall }: { plugin: AdminPlugin; management: boolean; installing: boolean; onInstall: () => void }) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [apiKey, setApiKey] = useState("");
  const configField = plugin.configFields.find((field) => field.key === "apiKey");
  const save = useMutation({
    mutationFn: () => api.updateAdminPluginConfig(plugin.id, apiKey),
    onSuccess: () => {
      setApiKey("");
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });
  const status = plugin.available ? availabilityLabel(plugin.configSource) : plugin.installed ? "暂不可用" : "尚未安装";
  const canOpen = plugin.configurable && plugin.configFields.length > 0;

  return (
    <article className="lux-admin-panel lux-admin-plugin-card">
      <button
        className="lux-admin-plugin-card-toggle"
        type="button"
        disabled={!canOpen}
        aria-expanded={canOpen ? open : undefined}
        onClick={() => canOpen && setOpen((value) => !value)}
      >
        <div className="lux-admin-plugin-icon" aria-hidden="true"><PackageOpen size={22} /></div>
        <div className="lux-admin-plugin-content"><div className="lux-admin-plugin-heading-line"><span className="lux-eyebrow">{plugin.id.toUpperCase()}</span><span className="lux-admin-plugin-category">{pluginCategoryLabel(plugin.category)}</span></div><h2>{plugin.name}</h2><p>{plugin.description}</p><div className={plugin.available ? "lux-admin-plugin-status is-ok" : "lux-admin-plugin-status is-warn"}>{plugin.available ? <CheckCircle2 size={15} /> : <AlertTriangle size={15} />}<span>{status}</span>{plugin.status ? <span>· {plugin.status}</span> : null}</div><small className="lux-admin-plugin-meta">{plugin.version ? `v${plugin.version}` : "内置"}{plugin.runtime ? ` · ${plugin.runtime}` : ""}</small>{plugin.capabilities?.length ? <small className="lux-admin-plugin-meta">能力：{plugin.capabilities.join("、")}</small> : null}{plugin.lastError ? <small className="lux-admin-plugin-error" role="alert">最近错误：{plugin.lastError}</small> : null}{canOpen ? <small><Settings2 size={13} /> 点击配置插件</small> : null}</div>
      </button>
      <button className="lux-button lux-button-primary" type="button" disabled={plugin.installed || installing} onClick={onInstall}>{management || plugin.installed ? <CheckCircle2 size={16} /> : <Download size={16} />}{installing ? "安装中…" : management || plugin.installed ? "已安装" : "安装插件"}</button>
      {open && configField ? <form className="lux-admin-plugin-config" autoComplete="off" onSubmit={(event) => { event.preventDefault(); save.mutate(); }}>
        <label>{configField.label}<input type={configField.type} value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder="留空可恢复内置 Key" autoComplete="new-password" /></label>
        <p>{configField.description} 当前：{availabilityLabel(plugin.configSource)}。</p>
        <button className="lux-button lux-button-secondary" type="submit" disabled={save.isPending}><Save size={15} /> {save.isPending ? "保存中…" : "保存配置"}</button>
        {save.error ? <span className="lux-error-copy" role="alert">{save.error.message}</span> : null}
      </form> : null}
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
