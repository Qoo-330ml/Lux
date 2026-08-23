import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, Database, HardDrive, ListChecks, Pencil, Settings2, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
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
  const [nameEditorOpen, setNameEditorOpen] = useState(false);
  const [draftServerName, setDraftServerName] = useState("");
  const nameEditorTriggerRef = useRef<HTMLButtonElement>(null);
  const nameEditorInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (dashboard.data) setServerName(dashboard.data.server.name);
  }, [dashboard.data]);

  useEffect(() => {
    if (typeof document === "undefined") return;

    const name = (serverName ?? dashboard.data?.server.name)?.trim();
    if (name) document.title = `${name} - Lux`;
  }, [dashboard.data?.server.name, serverName]);

  const openServerNameEditor = () => {
    setDraftServerName((serverName ?? dashboard.data?.server.name ?? "").trim());
    setNameEditorOpen(true);
  };

  const closeServerNameEditor = () => {
    setNameEditorOpen(false);
    nameEditorTriggerRef.current?.focus();
  };

  useEffect(() => {
    if (!nameEditorOpen) return;
    nameEditorInputRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeServerNameEditor();
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [nameEditorOpen]);

  const saveServerName = useMutation({
    mutationFn: () => api.updateAdminSettings({ serverName: draftServerName.trim() }),
    onSuccess: (settings) => {
      const nextName = settings.serverName?.trim() || draftServerName.trim();
      setServerName(nextName);
      queryClient.setQueryData<AdminDashboard>(queryKeys.adminDashboard, (current) => current
        ? { ...current, server: { ...current.server, name: nextName } }
        : current);
      closeServerNameEditor();
    },
  });

  if (dashboard.isPending) return <AdminState label="正在读取服务器状态…" />;
  if (dashboard.error) return <AdminState label={dashboard.error.message} error />;

  const { health, server, stats, nowPlaying, activity } = dashboard.data;
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
      </header>

      <section className="lux-admin-overview-card" aria-labelledby="server-overview-heading">
        <h2 className="lux-sr-only" id="server-overview-heading">服务器概况</h2>
        <div className="lux-admin-overview-top">
          <div className="lux-admin-overview-identity">
            <div className="lux-admin-overview-server-name-row">
              <span className="lux-admin-overview-server-name">{serverName ?? server.name}</span>
              <button ref={nameEditorTriggerRef} className="lux-admin-overview-name-edit" type="button" aria-label="编辑服务器名称" onClick={openServerNameEditor}><Pencil size={18} /></button>
            </div>
          </div>
          <div className={`lux-admin-overview-status${status ? " is-online" : " is-alert"}`}>
            {overviewStatus(health.status) ? <><i />{overviewStatus(health.status)}</> : null}
          </div>
          <OverviewInfo label="版本" value={`v${server.version}`} />
          <OverviewInfo label="运行时长" value={formatRuntime(health.runtime.seconds)} />
        </div>
        <div className="lux-admin-overview-metrics" aria-label="服务器概况指标">
          <OverviewMetric label="电影数量" value={formatCount(stats.movieCount)} />
          <OverviewMetric label="剧集数量" value={formatCount(stats.seriesCount)} />
          <OverviewMetric label="用户数量" value={formatCount(stats.userCount)} />
          <OverviewMetric label="CPU 占用" value={formatCpu(health.resources.cpu)} />
          <OverviewMetric label="内存占用" value={formatMemory(health.resources.memory)} />
          <OverviewMetric label="存储空间" value={formatStorage(health.resources.mediaStorage)} />
        </div>
      </section>
      {saveServerName.error ? <p className="lux-error-copy lux-dashboard-inline-error">{saveServerName.error.message}</p> : null}

      {nameEditorOpen ? (
        <div className="lux-server-name-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) closeServerNameEditor(); }}>
          <section className="lux-server-name-dialog" role="dialog" aria-modal="true" aria-labelledby="server-name-dialog-title">
            <button className="lux-server-name-dialog-close" type="button" aria-label="关闭服务器名称编辑" onClick={closeServerNameEditor}><X size={20} /></button>
            <form className="lux-server-name-dialog-form" onSubmit={(event) => { event.preventDefault(); if (draftServerName.trim()) saveServerName.mutate(); }}>
              <label id="server-name-dialog-title" htmlFor="server-name-dialog-input">服务器名称</label>
              <input ref={nameEditorInputRef} id="server-name-dialog-input" name="serverName" value={draftServerName} maxLength={80} aria-describedby="server-name-dialog-help" onChange={(event) => setDraftServerName(event.target.value)} />
              <p id="server-name-dialog-help">此名称用于标识此服务器。</p>
              {saveServerName.error ? <p className="lux-server-name-dialog-error" role="alert">{saveServerName.error.message}</p> : null}
              <button className="lux-server-name-dialog-save" type="submit" disabled={saveServerName.isPending || !draftServerName.trim()}>{saveServerName.isPending ? "保存中…" : "保存"}</button>
            </form>
          </section>
        </div>
      ) : null}

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
          <div className="lux-admin-meta-row"><span>Schema {health.schemaVersion}</span><span>{health.database.backend === "SQLITE" ? `SQLite ${health.database.journalMode.toUpperCase()}` : health.database.backend}</span></div>
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

function OverviewInfo({ label, value }: { label: string; value?: string }) {
  return <div className="lux-admin-overview-info" data-overview-value={label}><span><small>{label}：</small><strong aria-label={value ? undefined : `${label}数据未提供`}>{value ?? ""}</strong></span></div>;
}

function OverviewMetric({ label, value }: { label: string; value?: string }) {
  return <div className="lux-admin-overview-metric"><span><small>{label}</small><strong className="lux-admin-overview-metric-value" aria-label={value ? undefined : `${label}数据未提供`}>{value ?? ""}</strong></span></div>;
}

function overviewStatus(status: string) {
  if (status === "ok") return "在线";
  if (status === "degraded") return "异常";
  return "";
}

function formatRuntime(seconds: number | null | undefined) {
  if (!Number.isFinite(seconds) || (seconds ?? 0) < 0) return "不可用";
  let remaining = Math.floor(seconds ?? 0);
  const days = Math.floor(remaining / 86_400);
  remaining %= 86_400;
  const hours = Math.floor(remaining / 3_600);
  remaining %= 3_600;
  const minutes = Math.floor(remaining / 60);
  const secs = remaining % 60;
  return [days ? `${days}天` : "", hours ? `${hours}时` : "", minutes ? `${minutes}分` : "", `${secs}秒`]
    .filter(Boolean)
    .join(" ");
}

function formatCpu(cpu: AdminDashboard["health"]["resources"]["cpu"]) {
  if (!cpu.available) return "不可用";
  if (cpu.usageCores === null || cpu.capacityCores === null || cpu.usagePercent === null) return "采样中";
  return `${cpu.usageCores.toFixed(1)} / ${cpu.capacityCores.toFixed(1)} 核（${cpu.usagePercent.toFixed(1)}%）`;
}

function formatMemory(memory: AdminDashboard["health"]["resources"]["memory"]) {
  if (!memory.available || memory.usedBytes === null) return "不可用";
  const used = formatBytes(memory.usedBytes);
  return memory.limitBytes === null || memory.usagePercent === null
    ? used
    : `${used} / ${formatBytes(memory.limitBytes)}（${memory.usagePercent.toFixed(1)}%）`;
}

function formatStorage(storage: AdminDashboard["health"]["resources"]["mediaStorage"]) {
  if (!storage.available || storage.usedBytes === null || storage.totalBytes === null) return "不可用";
  return `${formatBytes(storage.usedBytes)} / ${formatBytes(storage.totalBytes)}`;
}

function formatCount(count: number) {
  if (!Number.isFinite(count) || count < 0) return "不可用";
  return Math.floor(count).toLocaleString("zh-CN");
}

function formatBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes < 0) return "不可用";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function AdminState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "控制台暂时不可用" : "正在加载控制台"}</h1><p>{label}</p></section>;
}
