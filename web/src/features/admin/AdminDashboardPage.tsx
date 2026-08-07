import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, Clapperboard, Clock3, Cpu, Database, Film, HardDrive, ListChecks, MemoryStick, Pencil, RefreshCw, Settings2, Tag, UsersRound } from "lucide-react";
import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { AdminDashboardActivity } from "./AdminDashboardActivity";
import { AdminDashboardNowPlaying } from "./AdminDashboardNowPlaying";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { AdminDashboard } from "../../lib/api/types";

export function AdminDashboardPage() {
  const queryClient = useQueryClient();
  const dashboard = useQuery({
    queryKey: queryKeys.adminDashboard,
    queryFn: () => api.adminDashboard(),
    refetchInterval: queryRefreshIntervals.liveDashboard,
  });
  const [serverName, setServerName] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    if (dashboard.data) setServerName(dashboard.data.server.name);
  }, [dashboard.data]);

  const saveServerName = useMutation({
    mutationFn: () => api.updateAdminSettings({ serverName: (serverName ?? server.name).trim() }),
    onSuccess: (settings) => {
      const nextName = settings.serverName?.trim() || (serverName ?? server.name).trim();
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
  const recentPlaybackActivity = activity
    .filter((event) => event.eventType !== "AUTH_LOGIN")
    .slice(0, 10);
  const checks = [
    { label: "数据库", ok: health.database.writable, detail: health.database.writable ? "可读写" : "不可写" },
    { label: "配置目录", ok: health.config.available && health.config.writable, detail: health.config.writable ? "可读写" : "不可写或不可用" },
    { label: "ffprobe", ok: health.ffprobe.available, detail: health.ffprobe.available ? "已就绪" : "未找到" },
    { label: "TMDb", ok: health.tmdb.configured, detail: health.tmdb.configured ? "已配置" : "未配置（可选）" },
  ];

  return (
    <div className="lux-admin-page lux-admin-dashboard-page">
      <header className="lux-admin-page-heading">
        <div><h1>控制台</h1></div>
        <button className="lux-button lux-button-secondary lux-admin-refresh" type="button" onClick={() => void dashboard.refetch()}><RefreshCw size={16} /> 刷新</button>
      </header>

      <section className="lux-admin-overview-card" aria-labelledby="server-overview-heading">
        <h2 className="lux-sr-only" id="server-overview-heading">服务器概况</h2>
        <div className="lux-admin-overview-top">
          <div className="lux-admin-overview-device" role="img" aria-label="服务器图片未提供" />
          <form className="lux-admin-overview-name-form" onSubmit={(event) => { event.preventDefault(); setSaved(false); if ((serverName ?? server.name).trim()) saveServerName.mutate(); }}>
            <label className="lux-sr-only" htmlFor="server-name">服务器名称</label>
            <input id="server-name" name="serverName" value={serverName ?? server.name} maxLength={80} aria-describedby="server-overview-heading" onChange={(event) => { setSaved(false); setServerName(event.target.value); }} />
            <button type="submit" aria-label={saved ? "服务器名称已保存" : "保存服务器名称"} disabled={saveServerName.isPending || !(serverName ?? server.name).trim()}><Pencil size={25} /></button>
          </form>
          <div className={`lux-admin-overview-status${status ? " is-online" : " is-alert"}`}>
            {overviewStatus(health.status) ? <><i />{overviewStatus(health.status)}</> : null}
          </div>
          <OverviewInfo icon={<Tag size={38} />} label="版本" value={`v${server.version}`} />
          <OverviewInfo icon={<Clock3 size={38} />} label="运行时长" />
          <div className="lux-admin-overview-action-slot" aria-hidden="true" />
        </div>
        <div className="lux-admin-overview-metrics" aria-label="服务器概况指标">
          <OverviewMetric icon={<Film size={38} />} label="电影数量" />
          <OverviewMetric icon={<Clapperboard size={38} />} label="剧集数量" />
          <OverviewMetric icon={<UsersRound size={38} />} label="用户数量" />
          <OverviewMetric icon={<Cpu size={38} />} label="CPU 占用" />
          <OverviewMetric icon={<MemoryStick size={38} />} label="内存占用" />
          <OverviewMetric icon={<HardDrive size={38} />} label="存储信息" />
        </div>
      </section>
      {saveServerName.error ? <p className="lux-error-copy lux-dashboard-inline-error">{saveServerName.error.message}</p> : null}

      <section className="lux-admin-dashboard-monitor-section" aria-labelledby="now-playing-heading">
        <div className="lux-admin-monitor-heading"><div><h2 id="now-playing-heading">正在播放</h2><p>实时查看每个账户的播放状态与直放链路。</p></div><span className="lux-admin-monitor-count">{nowPlaying.length} 个会话</span></div>
        <AdminDashboardNowPlaying sessions={nowPlaying} />
      </section>

      <section className="lux-admin-dashboard-monitor-section lux-admin-activity-section" aria-labelledby="activity-heading">
        <div className="lux-admin-monitor-heading"><div><h2 id="activity-heading">活跃状况</h2><p>开始播放、暂停和停止播放会按时间更新。</p></div><span className="lux-admin-monitor-count">最近 {recentPlaybackActivity.length} 条</span></div>
        <AdminDashboardActivity events={recentPlaybackActivity} />
      </section>

      <div className="lux-admin-dashboard-grid">
        <section className="lux-admin-panel">
          <div className="lux-admin-panel-heading"><div><h2>运行状态</h2></div><span className={status ? "lux-status-pill is-ok" : "lux-status-pill is-warn"}>{status ? "正常" : "降级"}</span></div>
          <div className="lux-admin-check-list">{checks.map((check) => <div className="lux-admin-check" key={check.label}><span className={check.ok ? "lux-check-icon is-ok" : "lux-check-icon is-warn"}>{check.ok ? <CheckCircle2 size={17} /> : <AlertTriangle size={17} />}</span><span>{check.label}</span><small>{check.detail}</small></div>)}</div>
          <div className="lux-admin-meta-row"><span>Schema {health.schemaVersion}</span><span>SQLite {health.database.journalMode.toUpperCase()}</span></div>
        </section>
        <section className="lux-admin-panel">
          <div className="lux-admin-panel-heading"><div><h2>管理入口</h2></div><HardDrive size={20} className="lux-admin-panel-icon" /></div>
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

function OverviewInfo({ icon, label, value }: { icon: ReactNode; label: string; value?: string }) {
  return <div className="lux-admin-overview-info" data-overview-value={label}><span className="lux-admin-overview-info-icon" aria-hidden="true">{icon}</span><span><small>{label}</small><strong aria-label={value ? undefined : `${label}数据未提供`}>{value ?? ""}</strong></span></div>;
}

function OverviewMetric({ icon, label, value }: { icon: ReactNode; label: string; value?: string }) {
  return <div className="lux-admin-overview-metric"><span className="lux-admin-overview-metric-icon" aria-hidden="true">{icon}</span><span><small>{label}</small><strong className="lux-admin-overview-metric-value" aria-label={value ? undefined : `${label}数据未提供`}>{value ?? ""}</strong></span></div>;
}

function overviewStatus(status: string) {
  if (status === "ok") return "在线";
  if (status === "degraded") return "异常";
  return "";
}

function AdminState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "控制台暂时不可用" : "正在加载控制台"}</h1><p>{label}</p></section>;
}
