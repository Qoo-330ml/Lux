import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, FileClock, RefreshCw, RotateCcw, StopCircle } from "lucide-react";
import { useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminJob } from "../../lib/api/types";
import { formatAdminDate } from "./date";

export function AdminOperationsPage() {
  const queryClient = useQueryClient();
  const [status, setStatus] = useState("");
  const jobs = useQuery({ queryKey: queryKeys.adminJobs(status), queryFn: () => api.adminJobs(status || undefined) });
  const logs = useQuery({ queryKey: queryKeys.adminLogs, queryFn: () => api.adminLogs() });
  const cancel = useMutation({ mutationFn: (jobId: string) => api.cancelAdminJob(jobId), onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "jobs"] }) });
  const retry = useMutation({ mutationFn: (jobId: string) => api.retryAdminJob(jobId), onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "jobs"] }) });

  if (jobs.isPending || logs.isPending) return <AdminOperationsState label="正在读取任务与日志…" />;
  if (jobs.error || logs.error) return <AdminOperationsState label={jobs.error?.message || logs.error?.message || "任务数据加载失败"} error />;
  const jobItems = jobs.data.jobs ?? [];
  const logItems = logs.data.events ?? [];

  return <div className="lux-admin-page"><header className="lux-admin-page-heading"><div><span className="lux-eyebrow">OPERATIONS</span><h1>任务与日志</h1><p>查看扫描队列、失败任务和管理员操作记录。</p></div><button className="lux-button lux-button-secondary" type="button" onClick={() => { void jobs.refetch(); void logs.refetch(); }}><RefreshCw size={16} /> 刷新</button></header><section className="lux-admin-panel lux-admin-operations-section"><div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">BACKGROUND JOBS</span><h2>后台任务</h2></div><select className="lux-admin-filter-select" value={status} onChange={(event) => setStatus(event.target.value)} aria-label="任务状态"><option value="">全部状态</option><option value="PENDING">等待中</option><option value="RUNNING">运行中</option><option value="FAILED">失败</option><option value="COMPLETED">已完成</option><option value="CANCELLED">已取消</option></select></div><div className="lux-admin-job-list">{jobItems.length === 0 ? <p className="lux-admin-muted">暂无任务记录。</p> : jobItems.map((job) => <JobRow key={job.id} job={job} onCancel={() => cancel.mutate(job.id)} onRetry={() => retry.mutate(job.id)} busy={cancel.isPending || retry.isPending} />)}</div></section><section className="lux-admin-panel"><div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">AUDIT TRAIL</span><h2>操作日志</h2></div><FileClock size={20} className="lux-admin-panel-icon" /></div><div className="lux-admin-log-list">{logItems.length === 0 ? <p className="lux-admin-muted">暂无操作日志。</p> : logItems.map((log) => <div className="lux-admin-log-row" key={log.id}><span className="lux-admin-log-icon"><FileClock size={15} /></span><div><strong>{log.eventType}</strong><small>{log.actorUsername || "系统"}{log.targetType ? ` · ${log.targetType}` : ""}{log.targetId ? ` · ${log.targetId}` : ""}</small></div><time>{formatAdminDate(log.createdAt)}</time></div>)}</div></section></div>;
}

function JobRow({ job, onCancel, onRetry, busy }: { job: AdminJob; onCancel: () => void; onRetry: () => void; busy: boolean }) {
  const progress = job.totalCount && job.totalCount > 0 ? Math.min(100, Math.round(((job.processedCount ?? 0) / job.totalCount) * 100)) : null;
  const active = job.status === "PENDING" || job.status === "RUNNING";
  return <article className="lux-admin-job-row"><div className={active ? "lux-job-icon is-active" : "lux-job-icon"}>{job.status === "FAILED" ? <AlertTriangle size={17} /> : job.status === "COMPLETED" ? <CheckCircle2 size={17} /> : <FileClock size={17} />}</div><div className="lux-admin-job-main"><div className="lux-admin-job-heading"><strong>{job.jobType}</strong><span className={`lux-job-status status-${job.status.toLowerCase()}`}>{job.status}</span></div><small>{job.libraryId} · {job.processedCount ?? 0}{job.totalCount ? ` / ${job.totalCount}` : ""}{job.error ? ` · ${job.error}` : ""}</small>{progress !== null ? <div className="lux-job-progress"><span style={{ width: `${progress}%` }} /></div> : null}</div><div className="lux-admin-job-actions">{active ? <button className="lux-icon-button lux-icon-button-small" type="button" aria-label="取消任务" onClick={onCancel} disabled={busy}><StopCircle size={15} /></button> : null}{job.status === "FAILED" || job.status === "CANCELLED" ? <button className="lux-icon-button lux-icon-button-small" type="button" aria-label="重试任务" onClick={onRetry} disabled={busy}><RotateCcw size={15} /></button> : null}</div></article>;
}

function AdminOperationsState({ label, error = false }: { label: string; error?: boolean }) { return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><span className="lux-eyebrow">LUX ADMIN</span><h1>{error ? "任务数据加载失败" : "正在加载任务"}</h1><p>{label}</p></section>; }
