import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Activity, FileClock, StopCircle } from "lucide-react";
import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminJob } from "../../lib/api/types";

const ACTIVE_STATUSES = ["RUNNING", "PENDING"] as const;

const JOB_TYPE_LABELS: Record<string, string> = {
  RECONCILE_LIBRARY: "全量校验",
  INCREMENTAL_SCAN: "实时扫描",
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
  const running = useQuery({
    queryKey: queryKeys.adminJobs("RUNNING"),
    queryFn: () => api.adminJobs("RUNNING"),
    refetchInterval: 5_000,
    refetchIntervalInBackground: false,
  });
  const pending = useQuery({
    queryKey: queryKeys.adminJobs("PENDING"),
    queryFn: () => api.adminJobs("PENDING"),
    refetchInterval: 5_000,
    refetchIntervalInBackground: false,
  });
  const libraries = useQuery({
    queryKey: queryKeys.adminLibraries,
    queryFn: () => api.adminLibraries(),
    staleTime: 60_000,
  });
  const cancel = useMutation({
    mutationFn: (jobId: string) => api.cancelAdminJob(jobId),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["admin", "jobs"] }),
  });
  const jobs = useMemo(() => {
    const active = [
      ...(running.data?.jobs ?? []),
      ...(pending.data?.jobs ?? []),
    ].filter((job) => ACTIVE_STATUSES.includes(job.status as "RUNNING" | "PENDING"));
    return active.sort(compareJobs);
  }, [pending.data?.jobs, running.data?.jobs]);
  const primary = jobs[0];
  const busy = running.isPending || pending.isPending;

  if (!primary && !busy) return null;

  return (
    <div className="lux-scan-activity">
      <button
        className={primary ? "lux-scan-activity-trigger is-active" : "lux-scan-activity-trigger"}
        type="button"
        aria-label={primary ? `后台扫描活动：${libraryLabel(primary, libraries.data?.libraries)}，${activityLabel(primary)}` : "后台扫描活动"}
        aria-expanded={open}
        title="后台扫描活动"
        onClick={() => setOpen((value) => !value)}
      >
        <Activity size={18} />
        {primary ? <span className="lux-scan-activity-dot" aria-hidden="true" /> : null}
      </button>
      {open ? (
        <div className="lux-scan-activity-popover" role="dialog" aria-label="后台扫描活动">
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
                    <button type="button" aria-label={`取消${activityLabel(job)}`} disabled={cancel.isPending || job.cancelRequested} onClick={() => cancel.mutate(job.id)}>
                      <StopCircle size={14} /> {job.cancelRequested ? "停止中" : "取消"}
                    </button>
                  </div>
                </article>
              ))}
            </div>
          ) : (
            <p className="lux-scan-activity-empty">正在读取后台活动</p>
          )}
        </div>
      ) : null}
    </div>
  );
}

function activityLabel(job: AdminJob) {
  return JOB_TYPE_LABELS[job.jobType] ?? "扫描任务";
}

function libraryLabel(job: AdminJob, libraries?: Array<{ id: string; name: string }>) {
  return libraries?.find((library) => library.id === job.libraryId)?.name ?? "未知媒体库";
}

function phaseLabel(job: AdminJob) {
  return PHASE_LABELS[job.scanPhase ?? "IDLE"] ?? "处理中";
}

function progressValue(job: AdminJob) {
  if (!job.totalCount || job.totalCount <= 0) return null;
  return Math.min(100, Math.round(((job.processedCount ?? 0) / job.totalCount) * 100));
}

function progressLabel(job: AdminJob) {
  const total = job.totalCount ?? 0;
  return total > 0 ? `${job.processedCount ?? 0}/${total}` : "发现中";
}

function compareJobs(left: AdminJob, right: AdminJob) {
  const status = Number(right.status === "RUNNING") - Number(left.status === "RUNNING");
  return status || String(right.createdAt ?? "").localeCompare(String(left.createdAt ?? ""));
}
