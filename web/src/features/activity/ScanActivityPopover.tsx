import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Activity, FileClock, StopCircle } from "lucide-react";
import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminTaskActivity } from "../../lib/api/types";

const JOB_TYPE_LABELS: Record<string, string> = {
  RECONCILE_LIBRARY: "全量校验",
  RECONCILIATION_SCAN: "全量校验",
  INCREMENTAL_SCAN: "实时扫描",
  FILL_MISSING: "元数据刮削",
  FULL_REFRESH: "元数据完整刷新",
  REIDENTIFY: "元数据匹配",
  STRM_MEDIA_INFO: "STRM 媒体信息",
  CHAPTER_DETECTION: "片头片尾检测",
  DANMAKU_MATCH: "弹幕匹配",
  AUTO_LIBRARY_COVER: "媒体库封面",
};

const PHASE_LABELS: Record<string, string> = {
  DISCOVERY: "发现目录",
  INDEXING: "处理文件",
  FINALIZING: "收尾同步",
  IDLE: "等待调度",
};

export function ScanActivityPopover() {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const activity = useQuery({
    queryKey: queryKeys.adminTaskActivity,
    queryFn: () => api.adminTaskActivity(),
    refetchInterval: 5_000,
    refetchIntervalInBackground: false,
  });
  const libraries = useQuery({
    queryKey: queryKeys.adminLibraries,
    queryFn: () => api.adminLibraries(),
    staleTime: 60_000,
  });
  const cancel = useMutation({
    mutationFn: (job: AdminTaskActivity) => cancelTask(job),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminTaskActivity }),
  });
  const jobs = useMemo(() => {
    const active = (activity.data?.activities ?? []).filter((job) =>
      job.status === "RUNNING" || job.status === "PENDING" || job.status === "QUEUED",
    );
    return active.sort(compareJobs);
  }, [activity.data?.activities]);
  const primary = jobs[0];
  const busy = activity.isPending;

  if (!primary && !busy) return null;

  return (
    <div className="lux-scan-activity">
      <button
        className={primary ? "lux-scan-activity-trigger is-active" : "lux-scan-activity-trigger"}
        type="button"
        aria-label={primary ? `后台任务活动：${libraryLabel(primary, libraries.data?.libraries)}，${activityLabel(primary)}` : "后台任务活动"}
        aria-expanded={open}
        title="后台任务活动"
        onClick={() => setOpen((value) => !value)}
      >
        <Activity size={18} />
        {primary ? <span className="lux-scan-activity-dot" aria-hidden="true" /> : null}
      </button>
      {open ? (
        <div className="lux-scan-activity-popover" role="dialog" aria-label="后台任务活动">
          <div className="lux-scan-activity-heading">
            <span><FileClock size={16} /> 正在处理</span>
            <strong>{jobs.length || 0}</strong>
          </div>
          {jobs.length ? (
            <div className="lux-scan-activity-list">
              {jobs.slice(0, 3).map((job) => (
                <article key={job.id} className="lux-scan-activity-row">
                  <div className="lux-scan-activity-row-heading">
                    <strong>{libraryLabel(job, libraries.data?.libraries)}</strong>
                    <span>{activityLabel(job)} · {progressLabel(job)}</span>
                  </div>
                  <p>{phaseLabel(job)}{job.currentItem ? ` · ${job.currentItem}` : ""}</p>
                  <div className="lux-scan-activity-progress" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={progressValue(job) ?? undefined}>
                    <span style={progressValue(job) == null ? undefined : { width: `${progressValue(job)}%` }} />
                  </div>
                  <div className="lux-scan-activity-actions">
                    <Link to="/admin/jobs" onClick={() => setOpen(false)}>任务与日志</Link>
                    {job.kind === "cover" ? null : <button type="button" aria-label={`取消${activityLabel(job)}`} disabled={cancel.isPending || job.cancelRequested} onClick={() => cancel.mutate(job)}>
                      <StopCircle size={14} /> {job.cancelRequested ? "停止中" : "取消"}
                    </button>}
                  </div>
                </article>
              ))}
            </div>
          ) : (
            <p className="lux-scan-activity-empty">正在读取后台任务活动</p>
          )}
        </div>
      ) : null}
    </div>
  );
}

function activityLabel(job: AdminTaskActivity) {
  return JOB_TYPE_LABELS[job.taskType] ?? "后台任务";
}

function libraryLabel(job: AdminTaskActivity, libraries?: Array<{ id: string; name: string }>) {
  return libraries?.find((library) => library.id === job.libraryId)?.name ?? "未知媒体库";
}

function phaseLabel(job: AdminTaskActivity) {
  if (job.kind === "cover") return job.status === "RUNNING" ? "生成封面" : "等待执行";
  return PHASE_LABELS[job.scanPhase ?? "IDLE"] ?? "处理中";
}

function progressValue(job: AdminTaskActivity) {
  if (!job.totalCount || job.totalCount <= 0) return null;
  return Math.min(100, Math.round(((job.processedCount ?? 0) / job.totalCount) * 100));
}

function progressLabel(job: AdminTaskActivity) {
  const total = job.totalCount ?? 0;
  return total > 0 ? `${job.processedCount ?? 0}/${total}` : "发现中";
}

function compareJobs(left: AdminTaskActivity, right: AdminTaskActivity) {
  const status = Number(right.status === "RUNNING") - Number(left.status === "RUNNING");
  return status || String(right.createdAt ?? "").localeCompare(String(left.createdAt ?? ""));
}

function cancelTask(job: AdminTaskActivity) {
  switch (job.kind) {
    case "scan": return api.cancelAdminJob(job.id);
    case "metadata": return api.cancelMetadataReidentify(job.id);
    case "strm": return api.cancelStrmProbeJob(job.id);
    case "chapter": return api.cancelChapterDetection(job.id);
    case "danmaku": return api.cancelDanmakuMatch(job.id);
    default: return Promise.reject(new Error("该任务暂不支持取消"));
  }
}
