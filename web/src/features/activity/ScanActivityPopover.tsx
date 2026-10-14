import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Activity, ChevronLeft, ChevronRight, FileClock, RefreshCw, StopCircle } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminLibraryPipelineLibrary, AdminLibraryPipelineStage, AdminTaskActivity } from "../../lib/api/types";

const LIBRARY_PIPELINE_PAGE_SIZE = 5;

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
  const [pipelinePage, setPipelinePage] = useState(1);
  const [expandedLibraryId, setExpandedLibraryId] = useState<string | null>(null);
  const activity = useQuery({
    queryKey: queryKeys.adminTaskActivity,
    queryFn: () => api.adminTaskActivity(),
    refetchInterval: open ? 5_000 : false,
    refetchIntervalInBackground: false,
  });
  const libraries = useQuery({
    queryKey: queryKeys.adminLibraries,
    queryFn: () => api.adminLibraries(),
    staleTime: 60_000,
  });
  const pipelineActivity = useQuery({
    queryKey: ["admin", "library-pipeline-activity", pipelinePage],
    queryFn: () => api.adminLibraryPipelineActivity(pipelinePage, LIBRARY_PIPELINE_PAGE_SIZE),
    refetchInterval: open ? 5_000 : false,
    refetchIntervalInBackground: false,
  });
  const cancel = useMutation({
    mutationFn: (job: AdminTaskActivity) => cancelTask(job),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminTaskActivity });
      void queryClient.invalidateQueries({ queryKey: ["admin", "library-pipeline-activity"] });
    },
  });
  const jobs = useMemo(() => {
    const active = (activity.data?.activities ?? []).filter((job) =>
      isActiveActivityJob(job),
    );
    return active.sort(compareJobs);
  }, [activity.data?.activities]);
  const primary = jobs[0];
  const pipelineLibraries = pipelineActivity.data?.libraries ?? [];
  const hasInProgressPipeline = pipelineLibraries.some((library) =>
    library.stages.some((stage) => stage.statusCounts.queued > 0 || stage.statusCounts.running > 0),
  );
  const isActive = jobs.length > 0 || hasInProgressPipeline;
  const busy = activity.isPending || pipelineActivity.isPending;
  const totalLibraries = pipelineActivity.data?.totalLibraries ?? 0;
  const totalPipelinePages = Math.max(1, Math.ceil(totalLibraries / LIBRARY_PIPELINE_PAGE_SIZE));

  useEffect(() => {
    if (!open) return undefined;
    const closeOnOutsidePointer = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && !rootRef.current?.contains(target)) setOpen(false);
    };
    document.addEventListener("pointerdown", closeOnOutsidePointer);
    return () => document.removeEventListener("pointerdown", closeOnOutsidePointer);
  }, [open]);

  if (!primary && !busy && !totalLibraries && !pipelineActivity.isError) return null;

  return (
    <div ref={rootRef} className="lux-scan-activity">
      <button
        className={isActive ? "lux-scan-activity-trigger is-active" : "lux-scan-activity-trigger"}
        type="button"
        aria-label={primary
          ? `后台任务活动：${libraryLabel(primary, libraries.data?.libraries)}，${activityLabel(primary)}`
          : isActive ? "媒体库入库阶段正在运行" : "后台任务活动"}
        aria-expanded={open}
        title="后台任务活动"
        onClick={() => setOpen((value) => !value)}
      >
        <Activity size={18} />
        {isActive ? <span className="lux-scan-activity-dot" aria-hidden="true" /> : null}
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
                    {!isPostprocessingActivityJob(job) && job.kind !== "cover" ? <button type="button" aria-label={`取消${activityLabel(job)}`} disabled={cancel.isPending || job.cancelRequested} onClick={() => cancel.mutate(job)}>
                      <StopCircle size={14} /> {job.cancelRequested ? "停止中" : "取消"}
                    </button> : null}
                  </div>
                </article>
              ))}
            </div>
          ) : (
            <p className="lux-scan-activity-empty">目前没有正在运行的单项作业</p>
          )}
          <section aria-label="媒体库入库阶段">
            <div className="lux-scan-activity-heading">
              <span>媒体库入库阶段</span>
              <strong>{totalLibraries}</strong>
            </div>
            {pipelineActivity.isPending ? (
              <p className="lux-scan-activity-empty" role="status">正在读取媒体库入库阶段</p>
            ) : pipelineActivity.isError ? (
              <div className="lux-scan-activity-empty" role="alert">
                <span>暂时无法读取媒体库入库阶段</span>
                <div className="lux-scan-activity-actions">
                  <button
                    type="button"
                    aria-label="重试读取入库阶段"
                    disabled={pipelineActivity.isFetching}
                    onClick={() => void pipelineActivity.refetch()}
                  >
                    <RefreshCw size={14} /> 重试
                  </button>
                </div>
              </div>
            ) : pipelineActivity.data?.libraries.length ? (
              <>
                <div className="lux-scan-activity-list">
                  {pipelineLibraries.map((library) => {
                    const expanded = expandedLibraryId === library.libraryId;
                    return (
                      <details
                        key={library.libraryId}
                        className="lux-scan-activity-row"
                        open={expanded}
                      >
                        <summary
                          className="lux-scan-activity-row-heading"
                          aria-label={`${expanded ? "收起" : "展开"}媒体库 ${library.libraryName}`}
                          onClick={(event) => {
                            event.preventDefault();
                            setExpandedLibraryId(expanded ? null : library.libraryId);
                          }}
                        >
                          <strong>{library.libraryName}</strong>
                          <span>{libraryPipelineSummary(library)}</span>
                        </summary>
                        {expanded ? (
                          <div className="lux-scan-activity-list">
                            {library.stages.map((stage) => (
                              <PipelineStageSummary key={stage.stage} stage={stage} />
                            ))}
                          </div>
                        ) : null}
                      </details>
                    );
                  })}
                </div>
                <div className="lux-scan-activity-actions">
                  <button
                    type="button"
                    aria-label="刷新入库阶段"
                    disabled={pipelineActivity.isFetching}
                    onClick={() => {
                      void Promise.all([pipelineActivity.refetch(), activity.refetch()]);
                    }}
                  >
                    <RefreshCw size={14} /> 刷新
                  </button>
                  {totalLibraries > LIBRARY_PIPELINE_PAGE_SIZE ? (
                    <>
                      <button
                        type="button"
                        aria-label="上一页媒体库"
                        disabled={pipelinePage <= 1 || pipelineActivity.isFetching}
                        onClick={() => {
                          setExpandedLibraryId(null);
                          setPipelinePage((page) => Math.max(1, page - 1));
                        }}
                      >
                        <ChevronLeft size={14} /> 上一页
                      </button>
                      <span aria-live="polite">{pipelinePage} / {totalPipelinePages}</span>
                      <button
                        type="button"
                        aria-label="下一页媒体库"
                        disabled={pipelinePage >= totalPipelinePages || pipelineActivity.isFetching}
                        onClick={() => {
                          setExpandedLibraryId(null);
                          setPipelinePage((page) => Math.min(totalPipelinePages, page + 1));
                        }}
                      >
                        下一页 <ChevronRight size={14} />
                      </button>
                    </>
                  ) : null}
                </div>
                {pipelineActivity.isFetching ? <p className="lux-scan-activity-empty" role="status">正在更新</p> : null}
              </>
            ) : (
              <p className="lux-scan-activity-empty">
                {totalLibraries ? "当前页没有媒体库" : "没有启用的媒体库"}
              </p>
            )}
          </section>
        </div>
      ) : null}
    </div>
  );
}

function PipelineStageSummary({ stage }: { stage: AdminLibraryPipelineStage }) {
  const label = PIPELINE_STAGE_LABELS[stage.stage];
  const status = pipelineStageStatusLabel(stage);
  return (
    <article
      className="lux-scan-activity-row"
      role="group"
      aria-label={`${label}：${status}`}
    >
      <div className="lux-scan-activity-row-heading">
        <strong>{label}</strong>
        <span>{status}</span>
      </div>
      <p>{recentActivityLabel(stage.recentActivityAt)}</p>
    </article>
  );
}

const PIPELINE_STAGE_LABELS: Record<AdminLibraryPipelineStage["stage"], string> = {
  INDEX: "索引扫描",
  LOCAL_METADATA: "本地元数据",
  PROBE: "媒体探测",
  THUMBNAIL: "视频缩略图",
  AUTOMATIC_METADATA: "在线刮削",
};

const PIPELINE_STATUS_LABELS = [
  ["queued", "排队"],
  ["running", "运行中"],
  ["completed", "完成"],
  ["failed", "失败"],
  ["cancelled", "已取消"],
] as const;

function pipelineStageStatusLabel(stage: AdminLibraryPipelineStage) {
  if (!stage.planned) return "未计划";
  const values = PIPELINE_STATUS_LABELS
    .filter(([status]) => stage.statusCounts[status] > 0)
    .map(([status, label]) => `${label} ${stage.statusCounts[status]}`);
  return values.length ? values.join(" · ") : "已计划，无状态计数";
}

function libraryPipelineSummary(library: AdminLibraryPipelineLibrary) {
  const plannedStages = library.stages.filter((stage) => stage.planned);
  if (!plannedStages.length) return "暂无计划";
  const statusLabels: string[] = [];
  for (const stage of plannedStages) {
    const activeOrProblemStatuses = PIPELINE_STATUS_LABELS
      .filter(([status]) => status !== "completed" && stage.statusCounts[status] > 0)
      .map(([, label]) => label);
    if (activeOrProblemStatuses.length) {
      statusLabels.push(`${PIPELINE_STAGE_LABELS[stage.stage]} ${activeOrProblemStatuses.join("/")}`);
    } else if (stage.statusCounts.completed > 0) {
      statusLabels.push(`${PIPELINE_STAGE_LABELS[stage.stage]} 已完成`);
    }
  }
  return statusLabels.length ? statusLabels.join(" · ") : "已计划，无待处理状态";
}

function recentActivityLabel(timestamp: number | null) {
  if (timestamp == null) return "暂无活动记录";
  const date = new Date(timestamp * 1000);
  if (Number.isNaN(date.getTime())) return "活动时间不可用";
  return `最近活动：${new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date)}`;
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
  if (isPostprocessingActivityJob(job)) return null;
  if (job.kind === "scan" && job.discoveryCompleted === false) return null;
  if (!job.totalCount || job.totalCount <= 0) return null;
  return Math.min(100, Math.round(((job.processedCount ?? 0) / job.totalCount) * 100));
}

function progressLabel(job: AdminTaskActivity) {
  if (isPostprocessingActivityJob(job)) return "索引完成，后处理进行中";
  if (job.kind === "scan" && job.discoveryCompleted === false) return "发现中";
  const total = job.totalCount ?? 0;
  return total > 0 ? `${job.processedCount ?? 0}/${total}` : "发现中";
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
