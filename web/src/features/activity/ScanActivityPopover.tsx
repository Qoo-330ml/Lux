import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Activity, FileClock, StopCircle } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminLibrary, AdminTaskActivity } from "../../lib/api/types";

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
  POSTPROCESSING: "索引已完成，后处理进行中",
  IDLE: "等待调度",
};

export function ScanActivityPopover() {
  const queryClient = useQueryClient();
  const rootRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const activity = useQuery({
    queryKey: queryKeys.adminTaskActivity,
    queryFn: () => api.adminTaskActivity(),
    refetchInterval: open ? 5_000 : false,
    refetchIntervalInBackground: false,
  });
  const jobs = useMemo(() => {
    const active = (activity.data?.activities ?? []).filter((job) =>
      isActiveActivityJob(job),
    );
    return active.sort(compareJobs);
  }, [activity.data?.activities]);
  const primary = jobs[0];
  const libraries = useQuery({
    queryKey: queryKeys.adminLibraries,
    queryFn: () => api.adminLibraries(),
    enabled: jobs.length > 0,
    staleTime: 60_000,
  });
  const cancel = useMutation({
    mutationFn: (job: AdminTaskActivity) => cancelTask(job),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminTaskActivity });
    },
  });

  useEffect(() => {
    if (!open) return undefined;
    const closeOnOutsidePointer = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && !rootRef.current?.contains(target)) setOpen(false);
    };
    document.addEventListener("pointerdown", closeOnOutsidePointer);
    return () => document.removeEventListener("pointerdown", closeOnOutsidePointer);
  }, [open]);

  useEffect(() => {
    if (!jobs.length && open) setOpen(false);
  }, [jobs.length, open]);

  if (!primary) return null;

  return (
    <div ref={rootRef} className="lux-scan-activity">
      <button
        className="lux-scan-activity-trigger is-active"
        type="button"
        aria-label={`后台任务活动：${libraryLabel(primary, libraries.data?.libraries)}，${activityLabel(primary)}`}
        aria-expanded={open}
        title="后台任务活动"
        onClick={() => setOpen((value) => !value)}
      >
        <Activity size={18} />
        <span className="lux-scan-activity-dot" aria-hidden="true" />
      </button>
      {open ? (
        <div className="lux-scan-activity-popover" role="dialog" aria-label="后台任务活动">
          <div className="lux-scan-activity-heading">
            <span><FileClock size={16} /> 正在处理</span>
            <strong>{jobs.length || 0}</strong>
          </div>
          <div className="lux-scan-activity-list">
            {jobs.slice(0, 3).map((job) => (
              <article key={job.id} className="lux-scan-activity-row">
                <div className="lux-scan-activity-row-heading">
                  <strong>{libraryLabel(job, libraries.data?.libraries)}</strong>
                  <span>{activityLabel(job)} · {progressLabel(job, libraries.data?.libraries)}</span>
                </div>
                <p>{phaseLabel(job)}{job.currentItem ? ` · ${job.currentItem}` : ""}</p>
                <div className="lux-scan-activity-progress" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={progressValue(job) ?? undefined}>
                  <span style={progressValue(job) == null ? undefined : { width: `${progressValue(job)}%` }} />
                </div>
                <div className="lux-scan-activity-actions">
                  <Link to="/admin/jobs" onClick={() => setOpen(false)}>任务与日志</Link>
                  {!isPostprocessingActivityJob(job) && job.kind !== "cover" ? <button type="button" aria-label={`取消${activityLabel(job)}`} disabled={cancel.isPending || job.cancelRequested} onClick={() => cancel.mutate(job)}>
                    <StopCircle size={14} /> {job.cancelRequested ? "停止中" : "取消"}
                  </button> : null}
                </div>
              </article>
            ))}
          </div>
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
  if (job.status === "PENDING" || job.status === "QUEUED") return "等待调度";
  if (job.kind === "cover") return job.status === "RUNNING" ? "生成封面" : "处理中";
  if (job.scanPhase) return PHASE_LABELS[job.scanPhase] ?? "处理中";
  return "处理中";
}

function progressValue(job: AdminTaskActivity) {
  if (isFullLibraryScan(job)) return null;
  if (isPostprocessingActivityJob(job)) return null;
  if (job.kind === "scan" && job.discoveryCompleted === false) return null;
  if (!job.totalCount || job.totalCount <= 0) return null;
  return Math.min(100, Math.round(((job.processedCount ?? 0) / job.totalCount) * 100));
}

function progressLabel(job: AdminTaskActivity, libraries?: Array<{ id: string; kind?: AdminLibrary["kind"] }>) {
  if (isFullLibraryScan(job)) {
    const kind = libraries?.find((library) => library.id === job.libraryId)?.kind;
    return formatProcessedMediaCount(job.processedMediaCount, kind);
  }
  if (isPostprocessingActivityJob(job)) return "索引完成，后处理进行中";
  if (job.kind === "scan" && job.discoveryCompleted === false) return "发现中";
  const total = job.totalCount ?? 0;
  return total > 0 ? `${job.processedCount ?? 0}/${total}` : "发现中";
}

function isFullLibraryScan(job: AdminTaskActivity) {
  return job.kind === "scan"
    && (job.taskType === "RECONCILE_LIBRARY" || job.taskType === "RECONCILIATION_SCAN");
}

function formatProcessedMediaCount(count: number | null | undefined, kind?: AdminLibrary["kind"]) {
  if (count == null) return "媒体进度不可用";
  const formattedCount = count.toLocaleString("zh-CN");
  if (kind === "MOVIE") return `已处理 ${formattedCount} 部电影`;
  if (kind === "SERIES") return `已处理 ${formattedCount} 部剧`;
  return `已处理 ${formattedCount} 项媒体`;
}

function compareJobs(left: AdminTaskActivity, right: AdminTaskActivity) {
  const status = Number(isActiveActivityJob(right)) - Number(isActiveActivityJob(left));
  return status || String(right.createdAt ?? "").localeCompare(String(left.createdAt ?? ""));
}

function isPostprocessingActivityJob(job: AdminTaskActivity) {
  return job.kind === "scan" && job.scanPhase === "POSTPROCESSING";
}

function isActiveActivityJob(job: AdminTaskActivity) {
  return job.status === "RUNNING"
    || job.status === "PENDING"
    || job.status === "QUEUED"
    || isPostprocessingActivityJob(job);
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
