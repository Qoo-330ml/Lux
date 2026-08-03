import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, Database, HardDrive, ListChecks, RefreshCw, Server, Settings2 } from "lucide-react";
import { Link } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";

export function AdminDashboardPage() {
  const health = useQuery({ queryKey: queryKeys.adminHealth, queryFn: () => api.adminHealth() });
  const libraries = useQuery({ queryKey: queryKeys.adminLibraries, queryFn: () => api.adminLibraries() });

  if (health.isPending || libraries.isPending) {
    return <AdminState label="正在读取服务器状态…" />;
  }
  if (health.error || libraries.error) {
    return <AdminState label={health.error?.message || libraries.error?.message || "管理数据加载失败"} error />;
  }

  const status = health.data.status === "ok";
  const libraryCount = libraries.data.libraries?.length ?? health.data.libraries.length;
  const checks = [
    { label: "数据库", ok: health.data.database.writable, detail: health.data.database.writable ? "可读写" : "不可写" },
    { label: "配置目录", ok: health.data.config.available && health.data.config.writable, detail: health.data.config.writable ? "可读写" : "不可写或不可用" },
    { label: "ffprobe", ok: health.data.ffprobe.available, detail: health.data.ffprobe.available ? "已就绪" : "未找到" },
    { label: "TMDb", ok: health.data.tmdb.configured, detail: health.data.tmdb.configured ? "已配置" : "未配置（可选）" },
  ];

  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading">
        <div><span className="lux-eyebrow">SERVER OVERVIEW</span><h1>仪表盘</h1><p>查看 Lux 服务、媒体库和后台任务的实时概况。</p></div>
        <button className="lux-button lux-button-secondary lux-admin-refresh" type="button" onClick={() => { void health.refetch(); void libraries.refetch(); }}><RefreshCw size={16} /> 刷新</button>
      </header>

      <section className="lux-admin-stat-grid" aria-label="服务器概览">
        <AdminStat icon={Server} label="服务状态" value={status ? "运行正常" : "需要关注"} tone={status ? "ok" : "warn"} />
        <AdminStat icon={Database} label="媒体库" value={`${libraryCount} 个`} />
        <AdminStat icon={ListChecks} label="运行中任务" value={`${health.data.jobs.scanRunning + health.data.jobs.metadataReidentifyRunning} 个`} />
        <AdminStat icon={AlertTriangle} label="失败扫描" value={`${health.data.jobs.scanFailed} 个`} tone={health.data.jobs.scanFailed ? "warn" : "ok"} />
      </section>

      <div className="lux-admin-dashboard-grid">
        <section className="lux-admin-panel">
          <div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">RUNTIME</span><h2>运行状态</h2></div><span className={status ? "lux-status-pill is-ok" : "lux-status-pill is-warn"}>{status ? "正常" : "降级"}</span></div>
          <div className="lux-admin-check-list">
            {checks.map((check) => <div className="lux-admin-check" key={check.label}><span className={check.ok ? "lux-check-icon is-ok" : "lux-check-icon is-warn"}>{check.ok ? <CheckCircle2 size={17} /> : <AlertTriangle size={17} />}</span><span>{check.label}</span><small>{check.detail}</small></div>)}
          </div>
          <div className="lux-admin-meta-row"><span>Schema {health.data.schemaVersion}</span><span>SQLite {health.data.database.journalMode.toUpperCase()}</span></div>
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
