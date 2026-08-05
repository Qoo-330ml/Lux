import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, FileClock, RefreshCw, RotateCcw, StopCircle } from "lucide-react";
import { Link } from "react-router-dom";
import { useState } from "react";
import { LuxSelect } from "../../components/LuxSelect";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminJob, AdminMetadataReidentifyJob } from "../../lib/api/types";
import { formatAdminDate } from "./date";
import { AdminScheduledTasksPanel } from "./AdminScheduledTasksPanel";

type OperationsJob = (AdminJob | AdminMetadataReidentifyJob) & {
  kind: "scan" | "metadata";
  jobType: string;
  libraryId?: string;
};

const JOB_STATUS_LABELS: Record<string, string> = {
  PENDING: "等待中",
  QUEUED: "等待中",
  RUNNING: "运行中",
  COMPLETED: "已完成",
  FAILED: "失败",
  CANCELLED: "已取消",
};

const JOB_TYPE_LABELS: Record<string, string> = {
  RECONCILE_LIBRARY: "媒体库扫描",
  SCAN: "媒体库扫描",
  TMDB_REIDENTIFY: "整库元数据匹配",
  METADATA_REIDENTIFY: "整库元数据匹配",
  REIDENTIFY: "整库元数据匹配",
  FILL_MISSING: "元数据仅补全",
  FULL_REFRESH: "元数据完整刷新",
};

const AUDIT_EVENT_LABELS: Record<string, string> = {
  METADATA_EDITED: "编辑元数据",
  SETTINGS_UPDATED: "更新服务端设置",
  LIBRARY_ACCESS_UPDATED: "更新媒体库权限",
  SCAN_STARTED: "开始扫描媒体库",
  SCAN_CANCELLED: "取消扫描任务",
  SCAN_RETRIED: "重试扫描任务",
  METADATA_REIDENTIFY_STARTED: "开始整库元数据匹配",
  METADATA_REIDENTIFY_RETRIED: "重试元数据匹配",
  METADATA_REFRESH_STARTED: "开始元数据刷新",
  METADATA_SEARCHED: "搜索元数据候选",
  METADATA_SELECTED: "确认元数据候选",
  USER_CREATED: "创建用户",
  USER_UPDATED: "更新用户",
  USER_DISABLED: "禁用用户",
  PLUGIN_INSTALLED: "安装插件",
  PLUGIN_CONFIG_UPDATED: "更新插件配置",
  LIBRARY_CREATED: "创建媒体库",
  LIBRARY_UPDATED: "更新媒体库",
  LIBRARY_COVER_UPDATED: "更新媒体库封面",
  LIBRARY_ROOT_ADDED: "添加媒体库路径",
  LIBRARY_ROOT_DELETED: "删除媒体库路径",
  LIBRARY_DELETED: "删除媒体库",
  SCHEDULE_UPDATED: "更新计划任务",
};

const ERROR_LABELS: Record<string, string> = {
  INVALID_ITEM_COUNT: "任务条目数量无效",
  INVALID_SEARCH: "无法从条目名称生成搜索条件",
  ITEM_NOT_FOUND: "媒体条目不存在",
  TMDB_UNAVAILABLE: "TMDb 服务暂时不可用",
  SCRAPER_UNAVAILABLE: "元数据刮削器不可用",
  CANDIDATE_ERROR: "候选搜索失败",
  LOW_CONFIDENCE: "匹配置信度不足，已转为待确认",
  METADATA_WRITE_FAILED: "元数据写回失败",
  METADATA_WRITE_UNAVAILABLE: "元数据写回服务不可用",
  STORAGE_ERROR: "数据库处理失败",
};

export function AdminOperationsPage() {
  const queryClient = useQueryClient();
  const [status, setStatus] = useState("");
  const jobs = useQuery({ queryKey: queryKeys.adminJobs(status), queryFn: () => api.adminJobs(status || undefined) });
  const metadataJobs = useQuery({
    queryKey: queryKeys.adminMetadataJobs(status),
    queryFn: () => api.adminMetadataReidentifyJobs(metadataStatusFilter(status)),
  });
  const logs = useQuery({ queryKey: queryKeys.adminLogs, queryFn: () => api.adminLogs() });
  const cancel = useMutation({ mutationFn: (jobId: string) => api.cancelAdminJob(jobId), onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "jobs"] }) });
  const cancelMetadata = useMutation({ mutationFn: (jobId: string) => api.cancelMetadataReidentify(jobId), onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "metadata-jobs"] }) });
  const retry = useMutation({ mutationFn: (jobId: string) => api.retryAdminJob(jobId), onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "jobs"] }) });
  const retryMetadata = useMutation({ mutationFn: (jobId: string) => api.retryMetadataReidentify(jobId), onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "metadata-jobs"] }) });

  if (jobs.isPending || metadataJobs.isPending || logs.isPending) return <AdminOperationsState label="正在读取任务与日志…" />;
  if (jobs.error || metadataJobs.error || logs.error) return <AdminOperationsState label={jobs.error?.message || metadataJobs.error?.message || logs.error?.message || "任务数据加载失败"} error />;

  const jobItems: OperationsJob[] = [
    ...(jobs.data.jobs ?? []).map((job): OperationsJob => ({ ...job, kind: "scan", jobType: job.jobType })),
    ...(metadataJobs.data.jobs ?? []).map((job): OperationsJob => ({ ...job, kind: "metadata", jobType: jobTypeForMetadataJob(job) })),
  ].sort((left, right) => Number(right.createdAt ?? 0) - Number(left.createdAt ?? 0));
  const logItems = logs.data.events ?? [];
  const refresh = () => {
    void jobs.refetch();
    void metadataJobs.refetch();
    void logs.refetch();
    void queryClient.invalidateQueries({ queryKey: ["admin", "scheduled-tasks"] });
    void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
  };

  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading">
        <div><span className="lux-eyebrow">LUX ADMIN</span><h1>任务与日志</h1><p>查看扫描、元数据匹配、元数据刷新和管理员操作记录。</p></div>
        <button className="lux-button lux-button-secondary" type="button" onClick={refresh}><RefreshCw size={16} /> 刷新</button>
      </header>
      <AdminScheduledTasksPanel />
      <section className="lux-admin-panel lux-admin-operations-section">
        <div className="lux-admin-panel-heading">
          <div><span className="lux-eyebrow">后台任务</span><h2>后台任务</h2></div>
          <LuxSelect className="lux-admin-filter-select" value={status} options={[{ value: "", label: "全部状态" }, { value: "PENDING", label: "等待中" }, { value: "RUNNING", label: "运行中" }, { value: "FAILED", label: "失败" }, { value: "COMPLETED", label: "已完成" }, { value: "CANCELLED", label: "已取消" }]} onChange={setStatus} aria-label="任务状态" />
        </div>
        <p className="lux-admin-muted">整库匹配会自动选择高置信度候选并按媒体库图像策略写回；低置信度条目请到 <Link to="/admin/metadata">元数据纠错</Link> 处理。指定条目的重新识别仍只生成候选。</p>
        <div className="lux-admin-job-list">{jobItems.length === 0 ? <p className="lux-admin-muted">暂无任务记录。</p> : jobItems.map((job) => <JobRow key={`${job.kind}-${job.id}`} job={job} onCancel={() => { if (job.kind === "metadata") cancelMetadata.mutate(job.id); else cancel.mutate(job.id); }} onRetry={() => { if (job.kind === "metadata") retryMetadata.mutate(job.id); else retry.mutate(job.id); }} busy={cancel.isPending || cancelMetadata.isPending || retry.isPending || retryMetadata.isPending} />)}</div>
      </section>
      <section className="lux-admin-panel">
        <div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">操作日志</span><h2>操作日志</h2></div><FileClock size={20} className="lux-admin-panel-icon" /></div>
        <div className="lux-admin-log-list">{logItems.length === 0 ? <p className="lux-admin-muted">暂无操作日志。</p> : logItems.map((log) => <div className="lux-admin-log-row" key={log.id}><span className="lux-admin-log-icon"><FileClock size={15} /></span><div><strong>{formatAuditEvent(log.eventType)}</strong><small>{log.actorUsername || "系统"}{log.targetType ? ` · ${formatTargetType(log.targetType)}` : ""}</small></div><time>{formatAdminDate(log.createdAt)}</time></div>)}</div>
      </section>
    </div>
  );
}

function metadataStatusFilter(status: string) {
  return status === "PENDING" ? "QUEUED" : status || undefined;
}

function jobTypeForMetadataJob(job: AdminMetadataReidentifyJob) {
  return job.mode === "REIDENTIFY" ? "METADATA_REIDENTIFY" : job.mode;
}

function formatJobType(jobType: string) {
  return JOB_TYPE_LABELS[jobType] || "后台任务";
}

function formatJobStatus(status: string) {
  return JOB_STATUS_LABELS[status] || "处理中";
}

function formatJobError(error?: string | null) {
  return error ? ERROR_LABELS[error] || "任务处理失败" : "";
}

function formatAuditEvent(eventType: string) {
  return AUDIT_EVENT_LABELS[eventType] || "系统操作记录";
}

function formatTargetType(targetType: string) {
  const labels: Record<string, string> = {
    library: "媒体库",
    scan_job: "扫描任务",
    metadata_reidentify_job: "元数据任务",
    settings: "服务端设置",
    user: "用户",
    plugin: "插件",
    scheduled_task: "计划任务",
  };
  return labels[targetType] || "系统对象";
}

function JobRow({ job, onCancel, onRetry, busy }: { job: OperationsJob; onCancel: () => void; onRetry: () => void; busy: boolean }) {
  const progress = job.totalCount && job.totalCount > 0 ? Math.min(100, Math.round(((job.processedCount ?? 0) / job.totalCount) * 100)) : null;
  const active = job.status === "PENDING" || job.status === "QUEUED" || job.status === "RUNNING";
  const retryable = job.status === "FAILED" || job.status === "CANCELLED";
  const error = formatJobError(job.error);
  return <article className="lux-admin-job-row"><div className={active ? "lux-job-icon is-active" : "lux-job-icon"}>{job.status === "FAILED" ? <AlertTriangle size={17} /> : job.status === "COMPLETED" ? <CheckCircle2 size={17} /> : <FileClock size={17} />}</div><div className="lux-admin-job-main"><div className="lux-admin-job-heading"><strong>{formatJobType(job.jobType)}</strong><span className={`lux-job-status status-${job.status.toLowerCase()}`}>{formatJobStatus(job.status)}</span></div><small>{job.kind === "metadata" ? "后台元数据任务" : "扫描任务"}{job.libraryId ? ` · 媒体库 ${job.libraryId}` : ""} · {job.processedCount ?? 0}{job.totalCount ? ` / ${job.totalCount}` : ""}{error ? ` · ${error}` : ""}</small>{progress !== null ? <div className="lux-job-progress"><span style={{ width: `${progress}%` }} /></div> : null}</div><div className="lux-admin-job-actions">{active ? <button className="lux-icon-button lux-icon-button-small" type="button" aria-label="取消任务" onClick={onCancel} disabled={busy}><StopCircle size={15} /></button> : null}{retryable ? <button className="lux-icon-button lux-icon-button-small" type="button" aria-label="重试任务" onClick={onRetry} disabled={busy}><RotateCcw size={15} /></button> : null}</div></article>;
}

function AdminOperationsState({ label, error = false }: { label: string; error?: boolean }) { return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><span className="lux-eyebrow">LUX ADMIN</span><h1>{error ? "任务数据加载失败" : "正在加载任务"}</h1><p>{label}</p></section>; }
