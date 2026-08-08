import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  CheckCircle2,
  ClipboardList,
  FileClock,
  Inbox,
  Pencil,
  Play,
  RefreshCw,
  RotateCcw,
  Save,
  StopCircle,
  X,
} from "lucide-react";
import { useMemo, useState, type FormEvent, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { LuxSelect } from "../../components/LuxSelect";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type {
  AdminAuditEvent,
  AdminJob,
  AdminMetadataReidentifyJob,
  AdminScheduledTask,
} from "../../lib/api/types";
import { formatAdminDate } from "./date";

type OperationsTab = "registered" | "runs" | "logs";
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
  RECONCILE_LIBRARY: "全量校验",
  SCAN: "媒体库扫描",
  INCREMENTAL_SCAN: "实时增量扫描",
  RECONCILIATION_SCAN: "全量校验",
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
  const [tab, setTab] = useState<OperationsTab>("registered");
  const [status, setStatus] = useState("");
  const [logLevel, setLogLevel] = useState("");
  const [logSearch, setLogSearch] = useState("");
  const [taskPage, setTaskPage] = useState(1);
  const tasks = useQuery({
    queryKey: queryKeys.adminScheduledTasks(taskPage),
    queryFn: () => api.adminScheduledTasks(taskPage),
  });
  const jobs = useQuery({
    queryKey: queryKeys.adminJobs(status),
    queryFn: () => api.adminJobs(status || undefined),
  });
  const metadataJobs = useQuery({
    queryKey: queryKeys.adminMetadataJobs(status),
    queryFn: () => api.adminMetadataReidentifyJobs(metadataStatusFilter(status)),
  });
  const logs = useQuery({ queryKey: queryKeys.adminLogs, queryFn: () => api.adminLogs() });
  const cancel = useMutation({
    mutationFn: (jobId: string) => api.cancelAdminJob(jobId),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "jobs"] }),
  });
  const cancelMetadata = useMutation({
    mutationFn: (jobId: string) => api.cancelMetadataReidentify(jobId),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "metadata-jobs"] }),
  });
  const retry = useMutation({
    mutationFn: (jobId: string) => api.retryAdminJob(jobId),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "jobs"] }),
  });
  const retryMetadata = useMutation({
    mutationFn: (jobId: string) => api.retryMetadataReidentify(jobId),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "metadata-jobs"] }),
  });

  const jobItems: OperationsJob[] = [
    ...(jobs.data?.jobs ?? []).map((job): OperationsJob => ({ ...job, kind: "scan", jobType: job.jobType })),
    ...(metadataJobs.data?.jobs ?? []).map((job): OperationsJob => ({
      ...job,
      kind: "metadata",
      jobType: jobTypeForMetadataJob(job),
    })),
  ].sort((left, right) => Number(right.createdAt ?? 0) - Number(left.createdAt ?? 0));
  const registeredTasks = tasks.data?.scheduledTasks ?? [];
  const logItems = useMemo(() => {
    const query = logSearch.trim().toLowerCase();
    return (logs.data?.events ?? []).filter((log) => {
      const matchesLevel = !logLevel || logLevel === auditLevel(log);
      const searchable = `${log.eventType} ${log.actorUsername ?? ""} ${log.targetType ?? ""}`.toLowerCase();
      return matchesLevel && (!query || searchable.includes(query));
    });
  }, [logLevel, logSearch, logs.data?.events]);

  if (tasks.isPending || jobs.isPending || metadataJobs.isPending || logs.isPending) {
    return <AdminOperationsState label="正在读取注册任务、运行记录与日志…" />;
  }
  if (tasks.error || jobs.error || metadataJobs.error || logs.error) {
    return <AdminOperationsState label={(tasks.error || jobs.error || metadataJobs.error || logs.error)?.message || "任务数据加载失败"} error />;
  }

  const runningCount = jobItems.filter((job) => isActiveJob(job.status)).length;
  const failedCount = jobItems.filter((job) => job.status === "FAILED").length;
  const enabledCount = registeredTasks.filter((task) => task.isEnabled && Boolean(task.schedule)).length;
  return (
    <div className="lux-admin-page lux-admin-operations-page">
      <header className="lux-admin-page-heading lux-operations-heading">
        <div><span className="lux-eyebrow">任务总览</span><h1>任务与日志</h1><p>所有后台工作都从系统或插件注册开始。</p></div>
      </header>

      <section className="lux-operations-summary" aria-label="任务概览">
        <OperationsStat label="已注册任务" value={tasks.data?.total ?? registeredTasks.length} detail="系统与插件提供" icon={<ClipboardList size={18} />} />
        <OperationsStat label="已启用" value={enabledCount} detail="已配置执行计划" icon={<CheckCircle2 size={18} />} />
        <OperationsStat label="正在运行" value={runningCount} detail="实时运行记录" icon={<RefreshCw size={18} />} />
        <OperationsStat label="失败记录" value={failedCount} detail="需要关注" icon={<AlertTriangle size={18} />} tone={failedCount ? "warn" : "default"} />
      </section>

      <nav className="lux-operations-tabs" aria-label="任务与日志分区" role="tablist">
        <OperationsTabButton active={tab === "registered"} onClick={() => setTab("registered")} label="已注册任务" count={tasks.data?.total ?? 0} />
        <OperationsTabButton active={tab === "runs"} onClick={() => setTab("runs")} label="运行记录" count={jobItems.length} />
        <OperationsTabButton active={tab === "logs"} onClick={() => setTab("logs")} label="系统日志" count={logItems.length} />
      </nav>

      {tab === "registered" ? (
        <RegisteredTasksSection
          tasks={registeredTasks}
          page={taskPage}
          pageSize={tasks.data?.pageSize ?? 100}
          total={tasks.data?.total ?? 0}
          onPageChange={setTaskPage}
          onRefresh={() => void tasks.refetch()}
        />
      ) : null}
      {tab === "runs" ? (
        <RunsSection
          jobs={jobItems}
          status={status}
          onStatusChange={setStatus}
          onCancel={(job) => job.kind === "metadata" ? cancelMetadata.mutate(job.id) : cancel.mutate(job.id)}
          onRetry={(job) => job.kind === "metadata" ? retryMetadata.mutate(job.id) : retry.mutate(job.id)}
          busy={cancel.isPending || cancelMetadata.isPending || retry.isPending || retryMetadata.isPending}
        />
      ) : null}
      {tab === "logs" ? (
        <LogsSection logs={logItems} level={logLevel} search={logSearch} onLevelChange={setLogLevel} onSearchChange={setLogSearch} />
      ) : null}
    </div>
  );
}

function RegisteredTasksSection({
  tasks,
  page,
  pageSize,
  total,
  onPageChange,
  onRefresh,
}: {
  tasks: AdminScheduledTask[];
  page: number;
  pageSize: number;
  total: number;
  onPageChange: (page: number) => void;
  onRefresh: () => void;
}) {
  return (
    <section className="lux-admin-panel lux-operations-section" aria-labelledby="registered-tasks-title">
      <div className="lux-operations-section-heading"><div><span className="lux-eyebrow">任务注册</span><h2 id="registered-tasks-title">已注册任务</h2><p>这里显示由 Lux 系统或插件注册的计划任务。实时增量扫描由文件系统监听触发，不在此配置。</p></div><span className="lux-operations-source-note">注册项自动持久化</span></div>
      {tasks.length === 0 ? <RegisteredTasksEmpty /> : <div className="lux-registered-task-list">{tasks.map((task) => <RegisteredTaskRow key={task.id ?? `${task.ownerType}:${task.ownerId}:${task.taskType}`} task={task} onSaved={onRefresh} />)}</div>}
      {tasks.length > 0 && total > pageSize ? <Pagination page={page} pageSize={pageSize} total={total} onPageChange={onPageChange} /> : null}
    </section>
  );
}

function RegisteredTasksEmpty() {
  return <div className="lux-operations-empty" role="status"><span className="lux-operations-empty-icon"><Inbox size={22} /></span><div><strong>还没有注册任务</strong><p>当系统功能或插件注册后台工作时，任务会自动出现在这里。</p></div></div>;
}

function RegisteredTaskRow({ task, onSaved }: { task: AdminScheduledTask; onSaved: () => void }) {
  const queryClient = useQueryClient();
  const [editing, setEditing] = useState(false);
  const [schedule, setSchedule] = useState(task.schedule ?? "");
  const [enabled, setEnabled] = useState(task.isEnabled);
  const update = useMutation({
    mutationFn: () => api.updateAdminScheduledTask({
      ownerType: task.ownerType === "GLOBAL" ? "GLOBAL" : "LIBRARY",
      ownerId: task.ownerType === "GLOBAL" ? "global" : task.ownerId,
      taskType: task.taskType,
      schedule: schedule.trim() || null,
      isEnabled: enabled && Boolean(schedule.trim()),
    }),
    onSuccess: () => {
      setEditing(false);
      void queryClient.invalidateQueries({ queryKey: ["admin", "scheduled-tasks"] });
      onSaved();
    },
  });
  const runNow = useMutation({
    mutationFn: () => runRegisteredTask(task),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminJobs() });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminMetadataJobs() });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLogs });
      onSaved();
    },
  });
  const name = task.name || taskLabel(task.taskType);
  const configured = Boolean(task.schedule);
  const stateLabel = task.isEnabled && configured ? "已启用" : configured ? "已停用" : "未配置计划";
  const canRunNow = isRunnableTask(task);

  const beginEditing = () => {
    setSchedule(task.schedule ?? "");
    setEnabled(task.isEnabled);
    setEditing(true);
  };
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    update.mutate();
  };

  return (
    <article className={`lux-registered-task-row${editing ? " is-editing" : ""}`}>
      <div className="lux-registered-task-icon"><ClipboardList size={18} /></div>
      <div className="lux-registered-task-copy">
        <div className="lux-registered-task-heading"><strong>{name}</strong><span className={`lux-registered-task-status ${task.isEnabled && configured ? "is-enabled" : configured ? "is-disabled" : "is-unconfigured"}`}>{stateLabel}</span></div>
        <p>{task.description || "由后台注册的任务。"}</p>
        <div className="lux-registered-task-meta"><span>{task.ownerName || (task.ownerType === "GLOBAL" ? "全局" : "指定媒体库")}</span><span>{task.sourceType === "PLUGIN" ? `插件注册${task.pluginId ? ` · ${task.pluginId}` : ""}` : "系统注册"}</span><code>{task.taskType}</code></div>
        {editing ? <form className="lux-registered-task-editor" onSubmit={submit}><label htmlFor={`schedule-${task.id ?? task.taskType}`}>执行计划<input id={`schedule-${task.id ?? task.taskType}`} value={schedule} onChange={(event) => setSchedule(event.target.value)} placeholder="例如 interval:1h" maxLength={128} /></label><label className="lux-admin-toggle" htmlFor={`enabled-${task.id ?? task.taskType}`}><input id={`enabled-${task.id ?? task.taskType}`} type="checkbox" checked={enabled} onChange={(event) => setEnabled(event.target.checked)} /><span>启用此任务</span></label><div className="lux-registered-task-editor-actions"><button className="lux-button lux-button-secondary" type="submit" disabled={update.isPending}><Save size={15} />{update.isPending ? "保存中…" : "保存"}</button><button className="lux-icon-button lux-icon-button-small" type="button" aria-label="取消编辑任务" onClick={() => setEditing(false)}><X size={16} /></button></div>{update.error ? <p className="lux-error-copy" role="alert">{update.error.message}</p> : null}</form> : <span className="lux-registered-task-schedule">{task.schedule || "尚未配置执行计划"}</span>}
      </div>
      {!editing ? <div className="lux-registered-task-actions">
        {canRunNow ? <button className="lux-button lux-button-secondary lux-registered-task-run" type="button" aria-label={`立即执行${name}`} onClick={() => runNow.mutate()} disabled={runNow.isPending}><Play size={14} />{runNow.isPending ? "执行中…" : "立即执行"}</button> : null}
        <button className="lux-icon-button lux-icon-button-small lux-registered-task-edit" type="button" aria-label={`编辑${name}`} onClick={beginEditing}><Pencil size={15} /></button>
      </div> : null}
      {runNow.error ? <p className="lux-error-copy lux-registered-task-error" role="alert">{runNow.error.message}</p> : null}
    </article>
  );
}

function RunsSection({
  jobs,
  status,
  onStatusChange,
  onCancel,
  onRetry,
  busy,
}: {
  jobs: OperationsJob[];
  status: string;
  onStatusChange: (status: string) => void;
  onCancel: (job: OperationsJob) => void;
  onRetry: (job: OperationsJob) => void;
  busy: boolean;
}) {
  return <section className="lux-admin-panel lux-operations-section" aria-labelledby="runs-title"><div className="lux-operations-section-heading"><div><span className="lux-eyebrow">执行历史</span><h2 id="runs-title">运行记录</h2><p>查看后台任务进度；失败或取消的任务可以从这里重试。</p></div><LuxSelect className="lux-admin-filter-select" value={status} options={[{ value: "", label: "全部状态" }, { value: "PENDING", label: "等待中" }, { value: "RUNNING", label: "运行中" }, { value: "FAILED", label: "失败" }, { value: "COMPLETED", label: "已完成" }, { value: "CANCELLED", label: "已取消" }]} onChange={onStatusChange} aria-label="运行记录状态" /></div><p className="lux-admin-muted">整库元数据操作会调用所属媒体库的注册刮削任务，低置信度条目请到 <Link to="/admin/metadata">元数据纠错</Link> 处理。</p><div className="lux-admin-job-list">{jobs.length === 0 ? <div className="lux-operations-empty" role="status"><span className="lux-operations-empty-icon"><Inbox size={22} /></span><div><strong>暂无运行记录</strong><p>手动或计划任务开始执行后，状态会显示在这里。</p></div></div> : jobs.map((job) => <JobRow key={`${job.kind}-${job.id}`} job={job} onCancel={() => onCancel(job)} onRetry={() => onRetry(job)} busy={busy} />)}</div></section>;
}

function LogsSection({ logs, level, search, onLevelChange, onSearchChange }: { logs: AdminAuditEvent[]; level: string; search: string; onLevelChange: (level: string) => void; onSearchChange: (search: string) => void }) {
  return <section className="lux-admin-panel lux-operations-section" aria-labelledby="logs-title"><div className="lux-operations-section-heading"><div><span className="lux-eyebrow">审计记录</span><h2 id="logs-title">系统日志</h2><p>管理员操作以脱敏事件形式记录，便于定位任务和权限变化。</p></div><FileClock size={20} className="lux-admin-panel-icon" /></div><div className="lux-operations-log-toolbar"><input aria-label="搜索系统日志" type="search" value={search} onChange={(event) => onSearchChange(event.target.value)} placeholder="搜索事件、操作者或对象" /><LuxSelect value={level} options={[{ value: "", label: "全部级别" }, { value: "INFO", label: "信息" }, { value: "WARN", label: "警告" }, { value: "ERROR", label: "错误" }]} onChange={onLevelChange} aria-label="日志级别" /></div><div className="lux-admin-log-list">{logs.length === 0 ? <div className="lux-operations-empty" role="status"><span className="lux-operations-empty-icon"><Inbox size={22} /></span><div><strong>暂无日志</strong><p>系统产生管理员事件后，会在这里保留脱敏记录。</p></div></div> : logs.map((log) => <LogRow key={log.id} log={log} />)}</div></section>;
}

function LogRow({ log }: { log: AdminAuditEvent }) {
  const [open, setOpen] = useState(false);
  const level = auditLevel(log);
  const metadata = log.metadata && Object.keys(log.metadata).length > 0 ? JSON.stringify(log.metadata, null, 2) : "";
  return <article className={`lux-operations-log-row is-${level.toLowerCase()}`}><span className="lux-operations-log-level">{level}</span><div className="lux-operations-log-copy"><strong>{formatAuditEvent(log.eventType)}</strong><p>{log.actorUsername || "系统"}{log.targetType ? ` · ${formatTargetType(log.targetType)}` : ""}{log.targetId ? ` · ${log.targetId}` : ""}</p>{open && metadata ? <pre>{metadata}</pre> : null}</div><time>{formatAdminDate(log.createdAt)}</time>{metadata ? <button className="lux-icon-button lux-icon-button-small" type="button" aria-expanded={open} aria-label={open ? "收起日志详情" : "展开日志详情"} onClick={() => setOpen((current) => !current)}>{open ? <X size={15} /> : <FileClock size={15} />}</button> : null}</article>;
}

function OperationsStat({ label, value, detail, icon, tone = "default" }: { label: string; value: number; detail: string; icon: ReactNode; tone?: "default" | "warn" }) {
  return <div className={`lux-operations-stat${tone === "warn" ? " is-warn" : ""}`}><span className="lux-operations-stat-icon">{icon}</span><div><small>{label}</small><strong>{value.toLocaleString("zh-CN")}</strong><p>{detail}</p></div></div>;
}

function OperationsTabButton({ active, label, count, onClick }: { active: boolean; label: string; count: number; onClick: () => void }) {
  return <button className={`lux-operations-tab${active ? " is-active" : ""}`} type="button" role="tab" aria-selected={active} onClick={onClick}><span>{label}</span><small>{count}</small></button>;
}

function Pagination({ page, pageSize, total, onPageChange }: { page: number; pageSize: number; total: number; onPageChange: (page: number) => void }) {
  const totalPages = Math.max(1, Math.ceil(total / pageSize));
  return <div className="lux-admin-pagination"><span>第 {page} / {totalPages} 页，共 {total} 项</span><div><button className="lux-button lux-button-secondary" type="button" onClick={() => onPageChange(Math.max(1, page - 1))} disabled={page === 1}>上一页</button><button className="lux-button lux-button-secondary" type="button" onClick={() => onPageChange(Math.min(totalPages, page + 1))} disabled={page === totalPages}>下一页</button></div></div>;
}

function JobRow({ job, onCancel, onRetry, busy }: { job: OperationsJob; onCancel: () => void; onRetry: () => void; busy: boolean }) {
  const progress = job.totalCount && job.totalCount > 0 ? Math.min(100, Math.round(((job.processedCount ?? 0) / job.totalCount) * 100)) : null;
  const active = isActiveJob(job.status);
  const retryable = job.status === "FAILED" || job.status === "CANCELLED";
  const error = formatJobError(job.error);
  return <article className="lux-admin-job-row"><div className={`lux-job-icon${active ? " is-active" : ""}`}>{job.status === "FAILED" ? <AlertTriangle size={17} /> : job.status === "COMPLETED" ? <CheckCircle2 size={17} /> : <FileClock size={17} />}</div><div className="lux-admin-job-main"><div className="lux-admin-job-heading"><strong>{formatJobType(job.jobType)}</strong><span className={`lux-job-status status-${job.status.toLowerCase()}`}>{formatJobStatus(job.status)}</span></div><small>{job.kind === "metadata" ? "元数据任务" : "扫描任务"}{job.libraryId ? ` · ${job.libraryId}` : ""} · {job.processedCount ?? 0}{job.totalCount ? ` / ${job.totalCount}` : ""}{error ? ` · ${error}` : ""}</small>{progress !== null ? <div className="lux-job-progress"><span style={{ width: `${progress}%` }} /></div> : null}</div><div className="lux-admin-job-actions">{active ? <button className="lux-icon-button lux-icon-button-small" type="button" aria-label="取消任务" onClick={onCancel} disabled={busy}><StopCircle size={15} /></button> : null}{retryable ? <button className="lux-icon-button lux-icon-button-small" type="button" aria-label="重试任务" onClick={onRetry} disabled={busy}><RotateCcw size={15} /></button> : null}</div></article>;
}

function metadataStatusFilter(status: string) {
  return status === "PENDING" ? "QUEUED" : status || undefined;
}

function jobTypeForMetadataJob(job: AdminMetadataReidentifyJob) {
  return job.mode === "REIDENTIFY" ? "METADATA_REIDENTIFY" : job.mode;
}

function taskLabel(taskType: string) {
  return JOB_TYPE_LABELS[taskType] || "后台任务";
}

function isRunnableTask(task: AdminScheduledTask) {
  return task.ownerType === "LIBRARY" && ["RECONCILIATION_SCAN", "METADATA_PARSE"].includes(task.taskType);
}

async function runRegisteredTask(task: AdminScheduledTask) {
  if (task.taskType === "RECONCILIATION_SCAN") {
    await api.startAdminScan(task.ownerId);
    return;
  }
  if (task.taskType === "METADATA_PARSE") {
    await api.startLibraryMetadataRefresh(task.ownerId, "FILL_MISSING");
    return;
  }
  return Promise.reject(new Error("该任务暂不支持立即执行"));
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
    scheduled_task: "注册任务",
  };
  return labels[targetType] || "系统对象";
}

function auditLevel(log: AdminAuditEvent) {
  if (/FAILED|ERROR|DENIED/.test(log.eventType)) return "ERROR";
  if (/CANCELLED|DISABLED/.test(log.eventType)) return "WARN";
  return "INFO";
}

function isActiveJob(status: string) {
  return status === "PENDING" || status === "QUEUED" || status === "RUNNING";
}

function AdminOperationsState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "任务数据加载失败" : "正在加载任务"}</h1><p>{label}</p></section>;
}
