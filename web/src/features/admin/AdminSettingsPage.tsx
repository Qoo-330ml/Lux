import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Save, Settings2 } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { AdminApiKeyPanel } from "../account/AdminApiKeyPanel";
import type {
  AdminNetworkProxySettings,
  NetworkProxyDiagnostics,
  NetworkProxyProbe,
} from "../../lib/api/types";

const emptyNetworkProxy: AdminNetworkProxySettings = {
  configured: false,
  url: null,
  hasCredentials: false,
  source: "none",
  restartRequired: true,
};

export function AdminSettingsPage() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const [minimumMinutes, setMinimumMinutes] = useState("2");
  const [showMetadataPending, setShowMetadataPending] = useState(true);
  const [proxyUrl, setProxyUrl] = useState("");
  const [saved, setSaved] = useState(false);
  const [proxySaved, setProxySaved] = useState(false);
  const [proxyDiagnostics, setProxyDiagnostics] = useState<NetworkProxyDiagnostics | null>(null);

  useEffect(() => {
    if (!settings.data) return;
    setMinimumMinutes(String(Math.round(settings.data.resumeMinTicks / 600000000)));
    setShowMetadataPending(settings.data.mediaStrategy.showMetadataPending ?? true);
    setProxyUrl(settings.data.networkProxy?.url ?? "");
  }, [settings.data]);

  const save = useMutation({
    mutationFn: () => api.updateAdminSettings({
      resumeMinTicks: Number(minimumMinutes) * 600000000,
      mediaStrategy: {
        ...settings.data.mediaStrategy,
        showMetadataPending,
      },
    }),
    onSuccess: (data) => {
      setMinimumMinutes(String(Math.round(data.resumeMinTicks / 600000000)));
      setSaved(true);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminSettings });
    },
    onError: () => setSaved(false),
  });

  const saveProxy = useMutation({
    mutationFn: () => api.updateAdminSettings({ networkProxyUrl: proxyUrl.trim() || null }),
    onSuccess: (data) => {
      setProxyUrl(data.networkProxy?.url ?? "");
      setProxyDiagnostics(null);
      setProxySaved(true);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminSettings });
    },
    onError: () => setProxySaved(false),
  });

  const testProxy = useMutation({
    mutationFn: () => api.testAdminNetworkProxy(proxyUrl.trim() || undefined),
    onSuccess: (data) => setProxyDiagnostics(data),
    onError: () => setProxyDiagnostics(null),
  });

  if (settings.isPending) return <AdminSettingsState label="正在读取服务器设置…" />;
  if (settings.error) return <AdminSettingsState label={settings.error.message} error />;

  const networkProxy = settings.data.networkProxy ?? emptyNetworkProxy;
  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading">
        <div>
          <h1>服务器设置</h1>
          <p>调整播放状态、网络访问和用户体验相关的全局策略。</p>
        </div>
      </header>

      <section className="lux-admin-panel lux-admin-settings-panel" aria-labelledby="admin-api-key-heading">
        <AdminApiKeyPanel />
      </section>

      <section className="lux-admin-panel lux-admin-settings-panel" aria-labelledby="playback-settings-heading">
        <div className="lux-admin-panel-heading">
          <div><h2 id="playback-settings-heading">播放行为</h2></div>
          <Settings2 size={20} className="lux-admin-panel-icon" />
        </div>
        <div className="lux-admin-settings-form">
          <label>
            <span>继续观看最小进度</span>
            <small>低于此播放时长的记录不会显示在“继续观看”。</small>
            <div className="lux-admin-input-with-suffix">
              <input type="number" min="0" value={minimumMinutes} onChange={(event) => { setSaved(false); setMinimumMinutes(event.target.value); }} />
              <em>分钟</em>
            </div>
          </label>
          <label className="lux-admin-toggle">
            <input
              type="checkbox"
              aria-label="显示媒体库待确认标记"
              checked={showMetadataPending}
              onChange={(event) => {
                setSaved(false);
                setShowMetadataPending(event.target.checked);
              }}
            />
            <span>显示媒体库待确认标记</span>
          </label>
          <small>关闭后只隐藏卡片上的标记，不会改变待确认状态或待确认筛选。</small>
          <button className="lux-button lux-button-primary lux-settings-save" type="button" disabled={save.isPending} onClick={() => save.mutate()}>
            <Save size={16} /> {save.isPending ? "保存中…" : "保存设置"}
          </button>
        </div>
        {saved ? <p className="lux-settings-saved"><Check size={15} /> 设置已保存</p> : null}
        {save.error ? <p className="lux-error-copy">{save.error.message}</p> : null}
      </section>

      <section className="lux-admin-panel lux-admin-settings-panel" aria-labelledby="network-proxy-heading">
        <div className="lux-admin-panel-heading">
          <div><h2 id="network-proxy-heading">网络代理设置</h2></div>
        </div>
        <div className="lux-admin-settings-form">
          <label>
            <span>代理地址</span>
            <small>支持 HTTP、HTTPS、SOCKS4、SOCKS4A、SOCKS5 和 SOCKS5H，例如 http://192.168.1.2:7890。</small>
            <input
              type="url"
              aria-label="网络代理地址"
              autoComplete="off"
              placeholder="http://192.168.1.2:7890"
              value={proxyUrl}
              onChange={(event) => {
                setProxySaved(false);
                setProxyDiagnostics(null);
                setProxyUrl(event.target.value);
              }}
            />
          </label>
          <p className="lux-network-proxy-help">
            {networkProxy.hasCredentials
              ? "代理认证信息已配置，页面不会显示密码；保存当前脱敏地址会保留现有认证信息。"
              : "如代理需要认证，可在地址中填写用户名；认证信息只保存在服务器配置文件中，不会返回到前端。"}
          </p>
          {networkProxy.source === "environment" && !networkProxy.url ? <p className="lux-network-proxy-status">当前代理由环境变量提供。</p> : null}
          <div className="lux-network-proxy-actions">
            <button className="lux-button lux-button-primary lux-settings-save" type="button" disabled={saveProxy.isPending} onClick={() => saveProxy.mutate()}>
              <Save size={16} /> {saveProxy.isPending ? "保存中…" : "保存网络代理"}
            </button>
            <button className="lux-button lux-button-secondary lux-settings-save" type="button" disabled={testProxy.isPending} onClick={() => testProxy.mutate()}>
              {testProxy.isPending ? "检测中…" : "检测延迟与出口"}
            </button>
          </div>
        </div>
        {proxySaved ? <p className="lux-settings-saved"><Check size={15} /> 网络代理设置已保存，重启 Lux 后生效。</p> : null}
        {saveProxy.error ? <p className="lux-error-copy">{saveProxy.error.message}</p> : null}
        {testProxy.error ? <p className="lux-error-copy">{testProxy.error.message}</p> : null}
        {proxyDiagnostics ? <NetworkProxyDiagnosticsView diagnostics={proxyDiagnostics} /> : null}
      </section>

      <section className="lux-admin-panel lux-admin-settings-note">
        <div className="lux-admin-panel-heading"><div><h2>服务信息</h2></div></div>
        <p>媒体扫描、元数据任务和数据库健康状态可在控制台的对应页面查看。Lux Web 与管理 API 使用同源会话和 CSRF 保护。</p>
      </section>
    </div>
  );
}

function NetworkProxyDiagnosticsView({ diagnostics }: { diagnostics: NetworkProxyDiagnostics }) {
  return (
    <div className="lux-network-proxy-diagnostics" role="status" aria-live="polite">
      <div className="lux-network-proxy-egress">
        <div><span>网络出口 IP</span><strong>{diagnostics.egressIp ?? "未获取"}</strong></div>
        <div><span>出口国家/地区</span><strong>{diagnostics.egressCountry ?? "未获取"}</strong></div>
      </div>
      <p className="lux-network-proxy-help">检测来源：{diagnostics.proxySource === "input" ? "当前输入地址" : diagnostics.proxySource === "settings" ? "已保存设置" : diagnostics.proxySource === "environment" ? "环境变量" : "当前直连配置"}。延迟为 Lux 服务端发起请求到收到响应的耗时。</p>
      <ul className="lux-network-proxy-probes" aria-label="网络代理延迟检测结果">
        {diagnostics.probes.map((probe) => <NetworkProxyProbeRow key={probe.id} probe={probe} />)}
      </ul>
    </div>
  );
}

function NetworkProxyProbeRow({ probe }: { probe: NetworkProxyProbe }) {
  const result = probe.reachable
    ? `${probe.latencyMs ?? "—"} ms${probe.status ? ` · HTTP ${probe.status}` : ""}`
    : `检测失败（${probe.error ?? "请求失败"}）`;
  return <li><span>{probe.label}</span><strong>{result}</strong></li>;
}

function AdminSettingsState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "设置加载失败" : "正在加载设置"}</h1><p>{label}</p></section>;
}
