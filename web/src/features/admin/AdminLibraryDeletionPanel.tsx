import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, FileClock, RotateCcw } from "lucide-react";
import { LuxIconButton } from "../../components/ui";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminLibraryDeletionJob } from "../../lib/api/types";
import { formatAdminDate } from "./date";

/**
 * 媒体库删除的后台进度与重试入口。
 *
 * 删除请求返回 `202` 后媒体库立即从可用列表消失，因此进度与重试放在媒体库页；
 * “任务与日志 → 运行记录”只保留已注册任务的执行历史，不再混入一次性删除任务。
 * 查询固定使用 `OPEN`（除 `COMPLETED` 外的全部状态），已完成的删除没有后续动作，
 * 不再占用界面。
 */
export function AdminLibraryDeletionPanel() {
  const queryClient = useQueryClient();
  const jobs = useQuery({
    queryKey: queryKeys.adminLibraryDeletionJobs("OPEN", 1),
    queryFn: () => api.adminLibraryDeletionJobs("OPEN", 1),
    // 有任务时跟随进度，没有任务时降低空闲轮询频率；删除请求受理后由媒体库页直接写入缓存。
    refetchInterval: (query) => (query.state.data?.jobs.length ? 5_000 : 15_000),
    retry: false,
  });
  const retry = useMutation({
    mutationFn: (jobId: string) => api.retryAdminLibraryDeletion(jobId),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "library-deletion-jobs"] }),
  });
  const items = jobs.data?.jobs ?? [];
  if (items.length === 0) return null;
  return <section className="lux-admin-panel lux-library-deletion-panel" aria-labelledby="library-deletion-jobs-title">
    <div className="lux-operations-section-heading"><div><span className="lux-eyebrow">后台清理</span><h2 id="library-deletion-jobs-title">正在删除的媒体库</h2><p>媒体库已从可用列表移除，数据库内容在后台分批清理，原媒体文件保留。</p></div></div>
    <div className="lux-admin-job-list">{items.map((job) => <LibraryDeletionJobRow key={job.id} job={job} retrying={retry.isPending} onRetry={() => retry.mutate(job.id)} />)}</div>
    {retry.error ? <p className="lux-error-copy" role="alert">{retry.error.message}</p> : null}
  </section>;
}

function LibraryDeletionJobRow({ job, retrying, onRetry }: { job: AdminLibraryDeletionJob; retrying: boolean; onRetry: () => void }) {
  const active = job.status === "PENDING" || job.status === "RUNNING";
  const retryable = job.status === "FAILED" || job.status === "CANCELLED";
  const message = job.errorCode ? libraryDeletionErrorLabel(job.errorCode) : null;
  return <article className="lux-admin-job-row">
    <div className={`lux-job-icon${active ? " is-active" : ""}`}>{job.status === "FAILED" ? <AlertTriangle size={17} /> : <FileClock size={17} />}</div>
    <div className="lux-admin-job-main">
      <div className="lux-admin-job-heading"><strong>{job.libraryName}</strong><span className={`lux-job-status status-${job.status.toLowerCase()}`}>{libraryDeletionStatusLabel(job.status)}</span></div>
      <div className="lux-admin-job-meta"><span className="lux-admin-job-context">{job.status === "PENDING" ? "等待后台清理" : libraryDeletionStageLabel(job.stage)}{job.processedCount > 0 ? ` · 已处理 ${job.processedCount.toLocaleString("zh-CN")} 条记录` : ""}</span></div>
      {message ? <div className="lux-admin-job-error" role="alert"><strong>{message}</strong><code>{job.errorCode}</code></div> : null}
      <time className="lux-admin-job-finished-at">{active ? `最近更新：${formatAdminDate(job.updatedAt)}` : `完成时间：${job.finishedAt == null ? "—" : formatAdminDate(job.finishedAt)}`}</time>
    </div>
    <div className="lux-admin-job-actions">{retryable ? <LuxIconButton className="lux-icon-button-small" type="button" aria-label={`重试删除媒体库“${job.libraryName}”`} onClick={onRetry} disabled={retrying}><RotateCcw size={15} /></LuxIconButton> : null}</div>
  </article>;
}

function libraryDeletionStatusLabel(status: AdminLibraryDeletionJob["status"]) {
  switch (status) {
    case "PENDING": return "等待中";
    case "RUNNING": return "清理中";
    case "COMPLETED": return "已删除";
    case "FAILED": return "失败";
    case "CANCELLED": return "已暂停";
    default: return "处理中";
  }
}

function libraryDeletionStageLabel(stage: string) {
  switch (stage) {
    case "CANCELLING_SCANS": return "正在停止扫描任务";
    case "MEDIA_ITEMS": return "正在清理媒体条目";
    case "FILESYSTEM_ENTRIES": return "正在清理文件索引";
    case "STRM_PROBE_JOBS": return "正在清理 STRM 探测任务";
    case "SCAN_JOB_DATA": return "正在清理扫描数据";
    case "SCAN_JOBS": return "正在清理扫描任务";
    case "FINALIZING": return "正在完成删除";
    case "COMPLETED": return "删除完成";
    default: return "正在后台清理";
  }
}

function libraryDeletionErrorLabel(errorCode: string) {
  switch (errorCode) {
    case "SERVER_SHUTDOWN": return "服务重启后任务已暂停，可重试继续清理。";
    case "SCAN_CANCEL_TIMEOUT": return "等待扫描停止超时，可重试清理。";
    case "SCAN_SHUTDOWN_FAILED": return "停止扫描任务失败，可重试清理。";
    case "DATABASE_ERROR": return "数据库清理失败，可重试。";
    case "PLUGIN_CLEANUP_FAILED": return "插件配置清理失败，可重试。";
    case "INVALID_LIBRARY_ID": return "删除任务中的媒体库 ID 无效，请联系管理员。";
    case "LIBRARY_RECORD_MISSING": return "媒体库记录已不存在，任务无法完成。";
    default: return "删除任务未完成，可重试；错误码：" + errorCode;
  }
}
