import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Clock3, Cpu, Database, MemoryStick, Pencil, Users, X } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { AdminDashboardActivity } from "./AdminDashboardActivity";
import { AdminDashboardNowPlaying } from "./AdminDashboardNowPlaying";
import { api } from "../../lib/api/client";
import { queryKeys, queryRefreshIntervals } from "../../lib/api/query-keys";
import type { AdminDashboard } from "../../lib/api/types";
import { LuxTextField } from "../../components/ui";
import { useAdminRealtimeConnectionState } from "./useAdminEvents";

export function AdminDashboardPage() {
  const queryClient = useQueryClient();
  // LUX-898: a connected admin channel must not switch the dashboard off. Uptime, CPU, memory and
  // storage are sampled per request, and the server only publishes the `dashboard` scope on
  // audit/activity events, so a quiet server would freeze them. Stretch the interval instead; the
  // task activity popover and the four job lists do stop, because `jobs` frames cover them.
  const adminRealtimeState = useAdminRealtimeConnectionState();
  const dashboard = useQuery({
    queryKey: queryKeys.adminDashboard,
    queryFn: () => api.adminDashboard(),
    refetchInterval: adminRealtimeState === "connected"
      ? queryRefreshIntervals.liveDashboardConnected
      : queryRefreshIntervals.liveDashboard,
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
  const memoryPercent = memoryUsagePercent(health.resources.memory);
  const recentPlaybackActivity = activity
    .filter((event) => event.eventType !== "AUTH_LOGIN")
    .slice(0, 10);
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
              <OverviewInfo
                className="lux-bento-hero-runtime"
                icon={<Clock3 className="lux-bento-inline-icon" size={14} strokeWidth={1.8} />}
                label="运行时长"
                value={formatRuntime(health.runtime.seconds)}
              />
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
          </div>

          {/* Media Count Cards */}
          <div className="lux-bento-card lux-bento-card-media-count">
            <div className="lux-bento-card-header">
              <span className="lux-bento-card-title">电影数量</span>
            </div>
            <div className="lux-bento-metric-body">
              <strong className="lux-admin-overview-metric-value lux-bento-big-num">{formatCount(stats.movieCount)}</strong>
            </div>
          </div>

          <div className="lux-bento-card lux-bento-card-media-count">
            <div className="lux-bento-card-header">
              <span className="lux-bento-card-title">剧集数量</span>
            </div>
            <div className="lux-bento-metric-body">
              <strong className="lux-admin-overview-metric-value lux-bento-big-num">{formatCount(stats.seriesCount)}</strong>
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

          {/* Resources: one card, three meters */}
          <div className="lux-bento-card lux-bento-card-resources">
            <div className="lux-bento-resource-list">
              <ResourceMeter
                title="CPU 占用"
                tile={<span className="lux-bento-icon-tile is-cpu" aria-hidden="true"><Cpu size={14} strokeWidth={1.8} /></span>}
                badge={health.resources.cpu.available && health.resources.cpu.usagePercent !== null
                  ? <span className="lux-bento-badge-accent">{health.resources.cpu.usagePercent.toFixed(1)}%</span>
                  : null}
                value={formatCpu(health.resources.cpu)}
                percent={health.resources.cpu.available ? health.resources.cpu.usagePercent : null}
                fillClass="is-cpu"
                progressLabel="CPU 使用率"
              />
              <ResourceMeter
                title="内存占用"
                tile={<span className="lux-bento-icon-tile is-memory" aria-hidden="true"><MemoryStick size={14} strokeWidth={1.8} /></span>}
                badge={memoryPercent !== null
                  ? <span className="lux-bento-badge-info">{memoryPercent.toFixed(1)}%</span>
                  : null}
                value={formatMemory(health.resources.memory)}
                percent={memoryPercent}
                fillClass="is-memory"
                progressLabel="内存使用率"
              />
              <ResourceMeter
                title="存储空间"
                tile={<span className="lux-bento-icon-tile is-storage" aria-hidden="true"><Database size={14} strokeWidth={1.8} /></span>}
                badge={health.resources.mediaStorage.available && health.resources.mediaStorage.usagePercent !== null
                  ? <span className="lux-bento-badge-purple">{health.resources.mediaStorage.usagePercent.toFixed(1)}% 已用</span>
                  : null}
                value={formatStorage(health.resources.mediaStorage)}
                percent={health.resources.mediaStorage.available ? health.resources.mediaStorage.usagePercent : null}
                fillClass="is-storage"
                progressLabel="存储空间使用率"
              />
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
              <LuxTextField ref={nameEditorInputRef} id="server-name-dialog-input" name="serverName" value={draftServerName} maxLength={80} aria-describedby="server-name-dialog-help" onChange={(event) => setDraftServerName(event.target.value)} />
              <p id="server-name-dialog-help">此名称用于标识此服务器。</p>
              {saveServerName.error ? <p className="lux-server-name-dialog-error" role="alert">{saveServerName.error.message}</p> : null}
              <button className="lux-server-name-dialog-save" type="submit" disabled={saveServerName.isPending || !draftServerName.trim()}>{saveServerName.isPending ? "保存中…" : "保存"}</button>
            </form>
          </section>
        </div>
      ) : null}

      <section className="lux-admin-dashboard-monitor-section" aria-labelledby="now-playing-heading">
        <div className="lux-admin-monitor-heading"><div><h2 id="now-playing-heading">正在播放</h2><p>实时查看每个账户的播放状态、播放方式和输出轨道。</p></div><span className="lux-admin-monitor-count">{nowPlaying.length} 个会话</span></div>
        <AdminDashboardNowPlaying sessions={nowPlaying} />
      </section>

      <section className="lux-admin-dashboard-monitor-section lux-admin-activity-section" aria-labelledby="activity-heading">
        <div className="lux-admin-monitor-heading"><div><h2 id="activity-heading">活跃状况</h2><p>开始播放、暂停和停止播放会按时间更新。</p></div><span className="lux-admin-monitor-count">最近 {recentPlaybackActivity.length} 条</span></div>
        <AdminDashboardActivity events={recentPlaybackActivity} />
      </section>

    </div>
  );
}

interface ResourceMeterProps {
  title: string;
  tile: ReactNode;
  badge?: ReactNode;
  value: string;
  percent?: number | null;
  fillClass?: string;
  progressLabel?: string;
}

function ResourceMeter({ title, tile, badge, value, percent = null, fillClass = "", progressLabel }: ResourceMeterProps) {
  return (
    <div className="lux-bento-meter">
      <div className="lux-bento-card-header">
        <span className="lux-bento-card-title">{tile}{title}</span>
        {badge}
      </div>
      <div className="lux-bento-metric-body">
        <strong className="lux-admin-overview-metric-value">{value}</strong>
        {percent !== null ? (
          <div className="lux-bento-progress-track">
            <div
              className={`lux-bento-progress-fill ${fillClass}`.trim()}
              role="progressbar"
              aria-label={progressLabel}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={percent}
              style={{ width: `${Math.min(100, Math.max(0, percent))}%` }}
            />
          </div>
        ) : null}
      </div>
    </div>
  );
}

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

type MemoryResource = AdminDashboard["health"]["resources"]["memory"];

/** The container limit wins; without one the host's physical memory is the capacity. */
function memoryCapacityBytes(memory: MemoryResource) {
  const capacity = memory.limitBytes ?? memory.hostTotalBytes;
  return capacity !== null && capacity > 0 ? capacity : null;
}

function memoryUsagePercent(memory: MemoryResource) {
  const capacity = memoryCapacityBytes(memory);
  if (!memory.available || memory.usedBytes === null || capacity === null) return null;
  return Math.min(100, (memory.usedBytes * 100) / capacity);
}

function formatMemory(memory: MemoryResource) {
  if (!memory.available || memory.usedBytes === null) return "不可用";
  const capacity = memoryCapacityBytes(memory);
  return capacity === null ? formatBytes(memory.usedBytes) : `${formatBytes(memory.usedBytes)} / ${formatBytes(capacity)}`;
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
