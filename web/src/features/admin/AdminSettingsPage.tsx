import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Save, Settings2 } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminNetworkProxySettings } from "../../lib/api/types";

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
  const [playedPercent, setPlayedPercent] = useState("90");
  const [minimumMinutes, setMinimumMinutes] = useState("2");
  const [proxyUrl, setProxyUrl] = useState("");
  const [saved, setSaved] = useState(false);
  const [proxySaved, setProxySaved] = useState(false);

  useEffect(() => {
    if (!settings.data) return;
    setPlayedPercent(String(settings.data.resumePlayedPercent));
    setMinimumMinutes(String(Math.round(settings.data.resumeMinTicks / 600000000)));
    setProxyUrl(settings.data.networkProxy?.url ?? "");
  }, [settings.data]);

  const save = useMutation({
    mutationFn: () => api.updateAdminSettings({
      resumePlayedPercent: Number(playedPercent),
      resumeMinTicks: Number(minimumMinutes) * 600000000,
    }),
    onSuccess: (data) => {
      setPlayedPercent(String(data.resumePlayedPercent));
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
      setProxySaved(true);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminSettings });
    },
    onError: () => setProxySaved(false),
  });

  if (settings.isPending) return <AdminSettingsState label="正在读取服务器设置…" />;
  if (settings.error) return <AdminSettingsState label={settings.error.message} error />;

  const networkProxy = settings.data.networkProxy ?? emptyNetworkProxy;
  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading">
        <div>
          <span className="lux-eyebrow">SERVER SETTINGS</span>
          <h1>服务器设置</h1>
          <p>调整播放状态、网络访问和用户体验相关的全局策略。</p>
        </div>
      </header>

      <section className="lux-admin-panel lux-admin-settings-panel" aria-labelledby="playback-settings-heading">
        <div className="lux-admin-panel-heading">
          <div><span className="lux-eyebrow">PLAYBACK</span><h2 id="playback-settings-heading">播放行为</h2></div>
          <Settings2 size={20} className="lux-admin-panel-icon" />
        </div>
        <div className="lux-admin-settings-form">
          <label>
            <span>标记为已看</span>
            <small>播放进度达到此百分比后，媒体会自动标记为已看。</small>
            <div className="lux-admin-input-with-suffix">
              <input type="number" min="1" max="100" value={playedPercent} onChange={(event) => { setSaved(false); setPlayedPercent(event.target.value); }} />
              <em>%</em>
            </div>
          </label>
          <label>
            <span>继续观看最小进度</span>
            <small>低于此播放时长的记录不会显示在“继续观看”。</small>
            <div className="lux-admin-input-with-suffix">
              <input type="number" min="0" value={minimumMinutes} onChange={(event) => { setSaved(false); setMinimumMinutes(event.target.value); }} />
              <em>分钟</em>
            </div>
          </label>
          <button className="lux-button lux-button-primary lux-settings-save" type="button" disabled={save.isPending} onClick={() => save.mutate()}>
            <Save size={16} /> {save.isPending ? "保存中…" : "保存设置"}
          </button>
        </div>
        {saved ? <p className="lux-settings-saved"><Check size={15} /> 设置已保存</p> : null}
        {save.error ? <p className="lux-error-copy">{save.error.message}</p> : null}
      </section>

      <section className="lux-admin-panel lux-admin-settings-panel" aria-labelledby="network-proxy-heading">
        <div className="lux-admin-panel-heading">
          <div><span className="lux-eyebrow">NETWORK</span><h2 id="network-proxy-heading">网络代理设置</h2></div>
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
              onChange={(event) => { setProxySaved(false); setProxyUrl(event.target.value); }}
            />
          </label>
          <p className="lux-network-proxy-help">
            {networkProxy.hasCredentials
              ? "代理认证信息已配置，页面不会显示密码；保存当前脱敏地址会保留现有认证信息。"
              : "如代理需要认证，可在地址中填写用户名；认证信息只保存在服务器配置文件中，不会返回到前端。"}
          </p>
          {networkProxy.source === "environment" && !networkProxy.url ? <p className="lux-network-proxy-status">当前代理由环境变量提供。</p> : null}
          <button className="lux-button lux-button-primary lux-settings-save" type="button" disabled={saveProxy.isPending} onClick={() => saveProxy.mutate()}>
            <Save size={16} /> {saveProxy.isPending ? "保存中…" : "保存网络代理"}
          </button>
        </div>
        {proxySaved ? <p className="lux-settings-saved"><Check size={15} /> 网络代理设置已保存，重启 Lux 后生效。</p> : null}
        {saveProxy.error ? <p className="lux-error-copy">{saveProxy.error.message}</p> : null}
      </section>

      <section className="lux-admin-panel lux-admin-settings-note">
        <div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">ABOUT THIS SERVER</span><h2>服务信息</h2></div></div>
        <p>媒体扫描、元数据任务和数据库健康状态可在控制台的对应页面查看。Lux Web 与管理 API 使用同源会话和 CSRF 保护。</p>
      </section>
    </div>
  );
}

function AdminSettingsState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><span className="lux-eyebrow">LUX ADMIN</span><h1>{error ? "设置加载失败" : "正在加载设置"}</h1><p>{label}</p></section>;
}
