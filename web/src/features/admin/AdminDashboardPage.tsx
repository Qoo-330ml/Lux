import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, Clock3, Cpu, Database, HardDrive, ListChecks, MemoryStick, Pencil, Server, Settings2, Users, X } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
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
  ];

  return (
    <div className="lux-admin-page lux-admin-dashboard-page">
      <header className="lux-admin-page-heading">
        <div><h1>控制台</h1></div>
      </header>

      <section className="lux-admin-overview-card" aria-labelledby="server-overview-heading">
        <h2 className="lux-sr-only" id="server-overview-heading">服务器概况</h2>
        <div className="lux-bento-grid" aria-label="服务器概况指标">
          {/* Hero Bento Card */}
          <div className="lux-bento-card lux-bento-card-hero">
            <div className="lux-bento-hero-top">
              <div className="lux-bento-badges">
                <div className={`lux-admin-overview-status${status ? " is-online" : " is-alert"}`}>
                  {overviewStatus(health.status) ? <><i />{overviewStatus(health.status)}</> : null}
                </div>
                <OverviewInfo className="lux-bento-version" label="版本" value={`v${server.version}`} />
              </div>
              <div className="lux-bento-hero-icon" aria-hidden="true">
                <Server size={20} />
              </div>
            </div>

            <div className="lux-admin-overview-identity">
              <div className="lux-admin-overview-server-name-row">
                <span className="lux-admin-overview-server-name">{serverName ?? server.name}</span>
                <button
                  ref={nameEditorTriggerRef}
                  className="lux-admin-overview-name-edit"
                  type="button"
                  aria-label="编辑服务器名称"
                  onClick={openServerNameEditor}
                >
                  <Pencil size={18} />
                </button>
              </div>
            </div>

            <div className="lux-bento-hero-footer">
              <OverviewInfo icon={<Clock3 className="lux-bento-inline-icon" size={14} strokeWidth={1.8} />} label="运行时长" value={formatRuntime(health.runtime.seconds)} />
            </div>
          </div>

          {/* Media Assets Bento Card */}
          <div className="lux-bento-card lux-bento-card-media">
            <div className="lux-bento-card-header">
              <span className="lux-bento-card-title">媒体库</span>
              <span className="lux-bento-card-meta">{formatCount(stats.movieCount + stats.seriesCount)} 项</span>
            </div>
            <div className="lux-bento-media-grid">
              <div className="lux-bento-media-subcard">
                <div className="lux-bento-subcard-header">
                  <small>电影数量</small>
                </div>
                <strong className="lux-admin-overview-metric-value">{formatCount(stats.movieCount)}</strong>
              </div>
              <div className="lux-bento-media-subcard">
                <div className="lux-bento-subcard-header">
                  <small>剧集数量</small>
                </div>
                <strong className="lux-admin-overview-metric-value">{formatCount(stats.seriesCount)}</strong>
              </div>
            </div>
            <div className="lux-bento-card-footer">
              <small>元数据已就绪</small>
              <Link to="/admin/libraries" className="lux-bento-link">进入媒体库 →</Link>
            </div>
          </div>

          {/* Users Card */}
          <div className="lux-bento-card lux-bento-card-users">
            <div className="lux-bento-card-header">
              <span className="lux-bento-card-title">用户数量</span>
              <span className="lux-bento-icon-tile is-users" aria-hidden="true"><Users size={14} strokeWidth={1.8} /></span>
            </div>
            <div className="lux-bento-metric-body">
              <strong className="lux-admin-overview-metric-value lux-bento-big-num">{formatCount(stats.userCount)}</strong>
            </div>
          </div>

          {/* CPU Card */}
          <div className="lux-bento-card lux-bento-card-cpu">
            <div className="lux-bento-card-header">
              <span className="lux-bento-card-title"><span className="lux-bento-icon-tile is-cpu" aria-hidden="true"><Cpu size={14} strokeWidth={1.8} /></span>CPU 占用</span>
              {health.resources.cpu.available && health.resources.cpu.usagePercent !== null ? (
                <span className="lux-bento-badge-accent">{health.resources.cpu.usagePercent.toFixed(1)}%</span>
              ) : null}
            </div>
            <div className="lux-bento-metric-body">
              <strong className="lux-admin-overview-metric-value">{formatCpu(health.resources.cpu)}</strong>
              {health.resources.cpu.available && health.resources.cpu.usagePercent !== null ? (
                <div className="lux-bento-progress-track">
                  <div
                    className="lux-bento-progress-fill is-cpu"
                    role="progressbar"
                    aria-label="CPU 使用率"
                    aria-valuemin={0}
                    aria-valuemax={100}
                    aria-valuenow={health.resources.cpu.usagePercent}
                    style={{ width: `${Math.min(100, Math.max(0, health.resources.cpu.usagePercent))}%` }}
                  />
                </div>
              ) : null}
            </div>
          </div>

          {/* Memory Card */}
          <div className="lux-bento-card lux-bento-card-mem">
            <div className="lux-bento-card-header">
              <span className="lux-bento-card-title">内存占用</span>
              <span className="lux-bento-icon-tile is-memory" aria-hidden="true"><MemoryStick size={14} strokeWidth={1.8} /></span>
            </div>
            <div className="lux-bento-metric-body">
              <strong className="lux-admin-overview-metric-value">{formatMemory(health.resources.memory)}</strong>
            </div>
          </div>

          {/* Storage Card */}
          <div className="lux-bento-card lux-bento-card-storage">
            <div className="lux-bento-card-header">
              <span className="lux-bento-card-title"><span className="lux-bento-icon-tile is-storage" aria-hidden="true"><Database size={14} strokeWidth={1.8} /></span>存储空间</span>
              {health.resources.mediaStorage.available && health.resources.mediaStorage.usagePercent !== null ? (
                <span className="lux-bento-badge-purple">{health.resources.mediaStorage.usagePercent.toFixed(1)}% 已用</span>
              ) : null}
            </div>
            <div className="lux-bento-metric-body">
              <strong className="lux-admin-overview-metric-value">{formatStorage(health.resources.mediaStorage)}</strong>
              {health.resources.mediaStorage.available && health.resources.mediaStorage.usagePercent !== null ? (
                <div className="lux-bento-progress-track">
                  <div
                    className="lux-bento-progress-fill is-storage"
                    role="progressbar"
                    aria-label="存储空间使用率"
                    aria-valuemin={0}
                    aria-valuemax={100}
                    aria-valuenow={health.resources.mediaStorage.usagePercent}
                    style={{ width: `${Math.min(100, Math.max(0, health.resources.mediaStorage.usagePercent))}%` }}
                  />
                </div>
              ) : null}
            </div>
          </div>
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

function OverviewInfo({ label, value, className = "", icon }: { label: string; value?: string; className?: string; icon?: ReactNode }) {
  return <div className={`lux-admin-overview-info ${className}`.trim()} data-overview-value={label}><span>{icon}<small>{label}：</small><strong aria-label={value ? undefined : `${label}数据未提供`}>{value ?? ""}</strong></span></div>;
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
  return `${cpu.usageCores.toFixed(1)} / ${cpu.capacityCores.toFixed(1)} 核`;
}

function formatMemory(memory: AdminDashboard["health"]["resources"]["memory"]) {
  if (!memory.available || memory.usedBytes === null) return "不可用";
  return formatBytes(memory.usedBytes);
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
