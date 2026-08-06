import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, Database, HardDrive, ListChecks, RefreshCw, Server, Settings2 } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { AdminDashboardActivity } from "./AdminDashboardActivity";
import { AdminDashboardNowPlaying } from "./AdminDashboardNowPlaying";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminDashboard } from "../../lib/api/types";

export function AdminDashboardPage() {
  const queryClient = useQueryClient();
  const dashboard = useQuery({
    queryKey: queryKeys.adminDashboard,
    queryFn: () => api.adminDashboard(),
    refetchInterval: 30_000,
  });
  const [serverName, setServerName] = useState("");
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    if (dashboard.data) setServerName(dashboard.data.server.name);
  }, [dashboard.data]);

  const saveServerName = useMutation({
    mutationFn: () => api.updateAdminSettings({ serverName: serverName.trim() }),
    onSuccess: (settings) => {
      const nextName = settings.serverName?.trim() || serverName.trim();
      setServerName(nextName);
      setSaved(true);
      queryClient.setQueryData<AdminDashboard>(queryKeys.adminDashboard, (current) => current
        ? { ...current, server: { ...current.server, name: nextName } }
        : current);
    },
    onError: () => setSaved(false),
  });

  if (dashboard.isPending) return <AdminState label="正在读取服务器状态…" />;
  if (dashboard.error) return <AdminState label={dashboard.error.message} error />;

  const { health, server, nowPlaying, activity } = dashboard.data;
  const status = health.status === "ok";
  const libraryCount = health.libraries.length;
  const checks = [
    { label: "数据库", ok: health.database.writable, detail: health.database.writable ? "可读写" : "不可写" },
    { label: "配置目录", ok: health.config.available && health.config.writable, detail: health.config.writable ? "可读写" : "不可写或不可用" },
    { label: "ffprobe", ok: health.ffprobe.available, detail: health.ffprobe.available ? "已就绪" : "未找到" },
    { label: "TMDb", ok: health.tmdb.configured, detail: health.tmdb.configured ? "已配置" : "未配置（可选）" },
  ];

  return (
    <div className="lux-admin-page lux-admin-dashboard-page">
      <header className="lux-admin-page-heading">
        <div><span className="lux-eyebrow">SERVER OVERVIEW</span><h1>仪表盘</h1><p>服务器状态、实时播放和账户活动，都在这里快速掌握。</p></div>
        <button className="lux-button lux-button-secondary lux-admin-refresh" type="button" onClick={() => void dashboard.refetch()}><RefreshCw size={16} /> 刷新</button>
      </header>

      <section className="lux-admin-server-identity" aria-labelledby="server-identity-heading">
        <div className="lux-admin-server-mark"><Server size={22} /></div>
        <div className="lux-admin-server-copy">
          <span className="lux-eyebrow">SERVER IDENTITY</span>
          <h2 id="server-identity-heading">{server.name}</h2>
          <div className="lux-admin-server-meta"><span>Lux Server</span><span>v{server.version}</span><span>Schema {server.schemaVersion}</span><span className="lux-admin-server-status"><i />{status ? "运行正常" : "需要关注"}</span></div>
        </div>
        <form className="lux-admin-server-form" onSubmit={(event) => { event.preventDefault(); setSaved(false); if (serverName.trim()) saveServerName.mutate(); }}>
          <label htmlFor="server-name"><span>服务器名称</span><input id="server-name" name="serverName" value={serverName} maxLength={80} onChange={(event) => { setSaved(false); setServerName(event.target.value); }} /></label>
          <button className="lux-button lux-button-secondary" type="submit" disabled={saveServerName.isPending || !serverName.trim()}>{saveServerName.isPending ? "保存中…" : saved ? "已保存" : "保存名称"}</button>
        </form>
      </section>
      {saveServerName.error ? <p className="lux-error-copy lux-dashboard-inline-error">{saveServerName.error.message}</p> : null}

      <section className="lux-admin-stat-grid" aria-label="服务器概览">
        <AdminStat icon={Server} label="服务状态" value={status ? "运行正常" : "需要关注"} tone={status ? "ok" : "warn"} />
        <AdminStat icon={Database} label="媒体库" value={`${libraryCount} 个`} />
        <AdminStat icon={ListChecks} label="运行中任务" value={`${health.jobs.scanRunning + health.jobs.metadataReidentifyRunning} 个`} />
        <AdminStat icon={AlertTriangle} label="失败扫描" value={`${health.jobs.scanFailed} 个`} tone={health.jobs.scanFailed ? "warn" : "ok"} />
      </section>

      <section className="lux-admin-dashboard-monitor-section" aria-labelledby="now-playing-heading">
        <div className="lux-admin-monitor-heading"><div><span className="lux-eyebrow">LIVE SESSIONS</span><h2 id="now-playing-heading">正在播放</h2><p>实时查看每个账户的播放状态与直放链路。</p></div><span className="lux-admin-monitor-count">{nowPlaying.length} 个会话</span></div>
        <AdminDashboardNowPlaying sessions={nowPlaying} />
      </section>

      <section className="lux-admin-dashboard-monitor-section lux-admin-activity-section" aria-labelledby="activity-heading">
        <div className="lux-admin-monitor-heading"><div><span className="lux-eyebrow">ACCOUNT ACTIVITY</span><h2 id="activity-heading">活跃状况</h2><p>登录、开始播放、暂停和停止播放会按时间更新。</p></div><span className="lux-admin-monitor-count">最近 {activity.length} 条</span></div>
        <AdminDashboardActivity events={activity} />
      </section>

      <div className="lux-admin-dashboard-grid">
        <section className="lux-admin-panel">
          <div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">RUNTIME</span><h2>运行状态</h2></div><span className={status ? "lux-status-pill is-ok" : "lux-status-pill is-warn"}>{status ? "正常" : "降级"}</span></div>
          <div className="lux-admin-check-list">{checks.map((check) => <div className="lux-admin-check" key={check.label}><span className={check.ok ? "lux-check-icon is-ok" : "lux-check-icon is-warn"}>{check.ok ? <CheckCircle2 size={17} /> : <AlertTriangle size={17} />}</span><span>{check.label}</span><small>{check.detail}</small></div>)}</div>
          <div className="lux-admin-meta-row"><span>Schema {health.schemaVersion}</span><span>SQLite {health.database.journalMode.toUpperCase()}</span></div>
        </section>
        <section className="lux-admin-panel">
          <div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">QUICK ACCESS</span><h2>管理入口</h2></div><HardDrive size={20} className="lux-admin-panel-icon" /></div>
          <div className="lux-admin-quick-links">
            <Link to="/admin/libraries"><Database size={17} /><span><strong>媒体库管理</strong><small>路径、扫描与计划</small></span></Link>
            <Link to="/admin/users"><ListChecks size={17} /><span><strong>用户与权限</strong><small>访问权限和设备策略</small></span></Link>
            <Link to="/admin/settings"><SettingsIcon /><span><strong>服务器设置</strong><small>播放和系统行为</small></span></Link>
          </div>
        </section>
      </div>
    </div>
  );
}

function SettingsIcon() { return <span className="lux-quick-icon"><Settings2 size={17} /></span>; }

function AdminStat({ icon: Icon, label, value, tone }: { icon: typeof Server; label: string; value: string; tone?: "ok" | "warn" }) {
  return <article className="lux-admin-stat"><span className={tone === "warn" ? "lux-admin-stat-icon is-warn" : "lux-admin-stat-icon"}><Icon size={19} /></span><div><small>{label}</small><strong>{value}</strong></div></article>;
}

function AdminState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><span className="lux-eyebrow">LUX ADMIN</span><h1>{error ? "控制台暂时不可用" : "正在加载控制台"}</h1><p>{label}</p></section>;
}
