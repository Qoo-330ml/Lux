import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, Download, PackageOpen, RefreshCw } from "lucide-react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminPlugin } from "../../lib/api/types";

export function AdminPluginsPage() {
  const queryClient = useQueryClient();
  const plugins = useQuery({ queryKey: queryKeys.adminPlugins, queryFn: () => api.adminPlugins() });
  const install = useMutation({
    mutationFn: (pluginId: string) => api.installAdminPlugin(pluginId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });

  if (plugins.isPending) return <AdminPluginsState label="正在读取插件库…" />;
  if (plugins.error) return <AdminPluginsState label={plugins.error.message} error />;

  const items = plugins.data.plugins ?? [];
  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading">
        <div><span className="lux-eyebrow">PLUGIN LIBRARY</span><h1>插件库</h1><p>安装已内置并经过验证的元数据插件，再为媒体库选择刮削器。</p></div>
        <button className="lux-button lux-button-secondary lux-admin-refresh" type="button" onClick={() => void plugins.refetch()}><RefreshCw size={16} /> 刷新</button>
      </header>
      <section className="lux-admin-plugin-grid" aria-label="可用插件">
        {items.length === 0 ? <div className="lux-admin-empty"><PackageOpen size={24} /><h2>暂无可用插件</h2><p>插件目录为空，请稍后重试。</p></div> : items.map((plugin) => <PluginCard key={plugin.id} plugin={plugin} installing={install.isPending && install.variables === plugin.id} onInstall={() => install.mutate(plugin.id)} />)}
      </section>
      {install.error ? <p className="lux-error-copy" role="alert">{install.error.message}</p> : null}
    </div>
  );
}

function PluginCard({ plugin, installing, onInstall }: { plugin: AdminPlugin; installing: boolean; onInstall: () => void }) {
  const status = plugin.available ? "可用于媒体库" : plugin.installed ? "需要配置 TMDb Token" : "尚未安装";
  return (
    <article className="lux-admin-panel lux-admin-plugin-card">
      <div className="lux-admin-plugin-icon" aria-hidden="true"><PackageOpen size={22} /></div>
      <div className="lux-admin-plugin-content"><span className="lux-eyebrow">{plugin.id.toUpperCase()}</span><h2>{plugin.name}</h2><p>{plugin.description}</p><div className={plugin.available ? "lux-admin-plugin-status is-ok" : "lux-admin-plugin-status is-warn"}>{plugin.available ? <CheckCircle2 size={15} /> : <AlertTriangle size={15} />}<span>{status}</span></div>{!plugin.configured ? <small>请先在初始化配置或环境变量中设置 TMDb Read Access Token。</small> : null}</div>
      <button className="lux-button lux-button-primary" type="button" disabled={plugin.installed || installing} onClick={onInstall}>{plugin.installed ? <CheckCircle2 size={16} /> : <Download size={16} />}{installing ? "安装中…" : plugin.installed ? "已安装" : "安装插件"}</button>
    </article>
  );
}

function AdminPluginsState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><span className="lux-eyebrow">PLUGIN LIBRARY</span><h1>{error ? "插件库加载失败" : "正在加载插件库"}</h1><p>{label}</p></section>;
}
