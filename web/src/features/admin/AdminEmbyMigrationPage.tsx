import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, CircleHelp, CloudDownload, LoaderCircle, RefreshCw, ShieldCheck, XCircle } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type {
  AdminEmbyMigrationImport,
  AdminEmbyMigrationJob,
  AdminEmbyMigrationMatch,
  AdminEmbyMigrationUserLink,
  EmbyMigrationSource,
} from "../../lib/api/types";

type ReportTab = "users" | "matches" | "imports";
type MigrationReportData = {
  page?: number;
  pageSize?: number;
  users?: AdminEmbyMigrationUserLink[] | (AdminEmbyMigrationUserLink | AdminEmbyMigrationMatch | AdminEmbyMigrationImport)[];
  matches?: AdminEmbyMigrationMatch[] | (AdminEmbyMigrationUserLink | AdminEmbyMigrationMatch | AdminEmbyMigrationImport)[];
  imports?: AdminEmbyMigrationImport[] | (AdminEmbyMigrationUserLink | AdminEmbyMigrationMatch | AdminEmbyMigrationImport)[];
};

const pageSize = 50;

export function AdminEmbyMigrationPage() {
  const queryClient = useQueryClient();
  const baseUrlInput = useRef<HTMLInputElement>(null);
  const apiKeyInput = useRef<HTMLInputElement>(null);
  const [allowPrivateNetwork, setAllowPrivateNetwork] = useState(false);
  const [dryRun, setDryRun] = useState(true);
  const [mergePolicy, setMergePolicy] = useState<"MERGE" | "OVERWRITE" | "SKIP">("MERGE");
  const [connection, setConnection] = useState<Awaited<ReturnType<typeof api.testAdminEmbyMigration>> | null>(null);
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);
  const [tab, setTab] = useState<ReportTab>("users");
  const [reportPage, setReportPage] = useState<Record<ReportTab, number>>({ users: 1, matches: 1, imports: 1 });

  const jobs = useQuery({
    queryKey: queryKeys.adminEmbyMigrations(),
    queryFn: () => api.adminEmbyMigrations(),
    refetchInterval: 5_000,
  });
  const selectedFromList = jobs.data?.jobs?.find((job) => job.id === selectedJobId) ?? null;
  const detail = useQuery({
    queryKey: queryKeys.adminEmbyMigration(selectedJobId ?? "none"),
    queryFn: () => api.adminEmbyMigration(selectedJobId!),
    enabled: Boolean(selectedJobId),
    refetchInterval: (query) => isActiveJob(query.state.data?.job) ? 5_000 : false,
  });
  const job = detail.data?.job ?? selectedFromList;

  useEffect(() => {
    if (!selectedJobId && jobs.data?.jobs?.[0]) setSelectedJobId(jobs.data.jobs[0].id);
  }, [jobs.data?.jobs, selectedJobId]);

  const readSource = (): EmbyMigrationSource => ({
    baseUrl: baseUrlInput.current?.value.trim() ?? "",
    apiKey: apiKeyInput.current?.value ?? "",
    allowPrivateNetwork,
  });

  const testConnection = useMutation({
    mutationFn: () => api.testAdminEmbyMigration(readSource()),
    onSuccess: setConnection,
  });
  const createJob = useMutation({
    mutationFn: () => api.createAdminEmbyMigration({ source: readSource(), dryRun, mergePolicy }),
    onSuccess: ({ job: created }) => {
      setSelectedJobId(created.id);
      setTab("users");
      setReportPage({ users: 1, matches: 1, imports: 1 });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminEmbyMigrations() });
    },
  });
  const cancelJob = useMutation({
    mutationFn: (jobId: string) => api.cancelAdminEmbyMigration(jobId),
    onSuccess: (_, jobId) => void queryClient.invalidateQueries({ queryKey: queryKeys.adminEmbyMigration(jobId) }),
  });
  const retryJob = useMutation({
    mutationFn: (jobId: string) => api.retryAdminEmbyMigration(jobId),
    onSuccess: (_, jobId) => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminEmbyMigration(jobId) });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminEmbyMigrations() });
    },
  });

  const report = useMigrationReport(selectedJobId, tab, reportPage[tab]);
  const canStart = Boolean(connection && !createJob.isPending);

  return (
    <div className="lux-admin-page lux-emby-migration-page">
      <header className="lux-emby-migration-heading">
        <div>
          <span className="lux-admin-eyebrow">只读来源 · Emby → Lux</span>
          <h1>Emby 迁移</h1>
          <p>把 Emby 的用户、收藏和聚合播放状态安全导入 Lux。迁移任务不会向 Emby 写入任何数据。</p>
        </div>
        <ShieldCheck size={30} aria-hidden="true" />
      </header>

      <section className="lux-admin-panel lux-emby-migration-panel" aria-labelledby="emby-connection-heading">
        <PanelHeading headingId="emby-connection-heading" icon={<CloudDownload size={19} />} title="连接 Emby" description="API Key 仅用于本次测试和迁移，关闭页面后不会保留。" />
        <div className="lux-emby-migration-form">
          <label>
            <span>Emby 地址</span>
            <input ref={baseUrlInput} id="emby-migration-base-url" type="url" autoComplete="url" placeholder="http://emby.local:8096" defaultValue="" onInput={() => setConnection(null)} />
          </label>
          <label>
            <span>API Key</span>
            <input ref={apiKeyInput} id="emby-migration-api-key" type="password" autoComplete="new-password" placeholder="粘贴 Emby API Key" defaultValue="" onInput={() => setConnection(null)} />
          </label>
          <label className="lux-admin-toggle lux-emby-private-toggle">
            <input type="checkbox" checked={allowPrivateNetwork} onChange={(event) => setAllowPrivateNetwork(event.target.checked)} />
            <span>允许连接局域网地址</span>
          </label>
          <button className="lux-button lux-button-secondary" type="button" aria-label="测试 Emby 连接" disabled={testConnection.isPending} onClick={() => testConnection.mutate()}>
            {testConnection.isPending ? <LoaderCircle size={16} className="lux-spin" /> : <RefreshCw size={16} />}
            {testConnection.isPending ? "测试中…" : "测试连接"}
          </button>
        </div>
        {testConnection.error ? <InlineError message={testConnection.error.message} /> : null}
        {connection ? <ConnectionResult connection={connection} /> : null}
      </section>

      <section className="lux-admin-panel lux-emby-migration-panel" aria-labelledby="emby-migration-settings-heading">
        <PanelHeading headingId="emby-migration-settings-heading" icon={<CloudDownload size={19} />} title="迁移设置" description="建议先用预览任务确认用户和媒体匹配结果。" />
        <div className="lux-emby-migration-settings">
          <label className="lux-admin-toggle lux-emby-dry-run">
            <input type="checkbox" checked={dryRun} onChange={(event) => setDryRun(event.target.checked)} />
            <span><strong>先创建预览任务（推荐）</strong><small>只读取并生成报告，不创建用户、不导入播放状态。</small></span>
          </label>
          <label>
            <span>已有播放状态</span>
            <select value={mergePolicy} onChange={(event) => setMergePolicy(event.target.value as typeof mergePolicy)}>
              <option value="MERGE">合并：保留两边更完整的状态</option>
              <option value="OVERWRITE">覆盖：使用 Emby 状态</option>
              <option value="SKIP">跳过：不修改已有状态</option>
            </select>
          </label>
          <button className="lux-button lux-button-primary" type="button" aria-label="开始 Emby 迁移" disabled={!canStart} onClick={() => createJob.mutate()}>
            <CloudDownload size={16} /> {createJob.isPending ? "创建中…" : dryRun ? "创建预览任务" : "开始正式迁移"}
          </button>
        </div>
        {createJob.error ? <InlineError message={createJob.error.message} /> : null}
        {createJob.data ? <p className="lux-emby-migration-success"><CheckCircle2 size={16} /> {createJob.data.job.dryRun ? "预览任务已创建" : "正式迁移任务已创建"}</p> : null}
      </section>

      <CapabilityNotice capability={connection?.historyCapability ?? job?.historyCapability} />

      <section className="lux-admin-panel lux-emby-migration-panel" aria-labelledby="emby-migration-jobs-heading">
        <PanelHeading headingId="emby-migration-jobs-heading" icon={<RefreshCw size={19} />} title="迁移任务" description="运行中的任务每 5 秒自动刷新。" />
        {jobs.isPending ? <LoadingState label="正在读取迁移任务…" /> : jobs.error ? <InlineError message={jobs.error.message} /> : jobs.data?.jobs?.length ? (
          <div className="lux-emby-job-list">
            {jobs.data.jobs.map((candidate) => <JobSummary key={candidate.id} job={candidate} selected={candidate.id === selectedJobId} onSelect={() => setSelectedJobId(candidate.id)} />)}
          </div>
        ) : <EmptyState label="还没有迁移任务" detail="测试连接后，可以从上方创建一个预览任务。" />}
      </section>

      {job ? <MigrationDetails
        job={job}
        tab={tab}
        report={report}
        onTabChange={(nextTab) => { setTab(nextTab); setReportPage((current) => ({ ...current, [nextTab]: 1 })); }}
        onPageChange={(page) => setReportPage((current) => ({ ...current, [tab]: page }))}
        onCancel={() => cancelJob.mutate(job.id)}
        onRetry={() => retryJob.mutate(job.id)}
        actionPending={cancelJob.isPending || retryJob.isPending}
      /> : null}
    </div>
  );
}

function useMigrationReport(jobId: string | null, tab: ReportTab, page: number) {
  return useQuery<MigrationReportData>({
    queryKey: jobId ? reportKey(tab, jobId, page) : ["admin", "emby-migration-report", "none", tab, page],
    queryFn: (): Promise<MigrationReportData> => {
      if (!jobId) return Promise.resolve({ page, pageSize, users: [], matches: [], imports: [] });
      if (tab === "users") return api.adminEmbyMigrationUsers(jobId, page);
      if (tab === "matches") return api.adminEmbyMigrationMatches(jobId, page);
      return api.adminEmbyMigrationImports(jobId, page);
    },
    enabled: Boolean(jobId),
    retry: false,
  });
}

function reportKey(tab: ReportTab, jobId: string, page: number) {
  if (tab === "users") return queryKeys.adminEmbyMigrationUsers(jobId, page);
  if (tab === "matches") return queryKeys.adminEmbyMigrationMatches(jobId, page);
  return queryKeys.adminEmbyMigrationImports(jobId, page);
}

function MigrationDetails({
  job,
  tab,
  report,
  onTabChange,
  onPageChange,
  onCancel,
  onRetry,
  actionPending,
}: {
  job: AdminEmbyMigrationJob;
  tab: ReportTab;
  report: ReturnType<typeof useMigrationReport>;
  onTabChange: (tab: ReportTab) => void;
  onPageChange: (page: number) => void;
  onCancel: () => void;
  onRetry: () => void;
  actionPending: boolean;
}) {
  const progress = job.totalCount > 0 ? Math.min(100, Math.round((job.processedCount / job.totalCount) * 100)) : 0;
  const active = isActiveJob(job);
  return (
    <section className="lux-admin-panel lux-emby-migration-panel" aria-labelledby="emby-migration-detail-heading">
      <div className="lux-admin-panel-heading">
        <div><span className="lux-admin-eyebrow">任务详情</span><h2 id="emby-migration-detail-heading">{job.sourceLabel}</h2><p>{statusLabel(job.status)} · {phaseLabel(job.phase)} · {job.dryRun ? "预览" : "正式迁移"}</p></div>
        <div className="lux-emby-job-actions">
          {active ? <button className="lux-button lux-button-secondary" type="button" disabled={actionPending || job.cancelRequested} onClick={onCancel}><XCircle size={15} /> {job.cancelRequested ? "取消中…" : "取消任务"}</button> : null}
          {job.status === "FAILED" || job.status === "CANCELLED" ? <button className="lux-button lux-button-secondary" type="button" disabled={actionPending} onClick={onRetry}><RefreshCw size={15} /> 重试</button> : null}
        </div>
      </div>
      <div className="lux-emby-job-progress" aria-label={`迁移进度 ${progress}%`}>
        <div><strong>{progress}%</strong><span>{job.processedCount.toLocaleString()} / {job.totalCount.toLocaleString()}</span></div>
        <div className="lux-emby-progress-track"><span style={{ width: `${progress}%` }} /></div>
      </div>
      <div className="lux-emby-job-stats"><Stat label="已匹配" value={job.matchedCount} /><Stat label="已跳过" value={job.skippedCount} /><Stat label="失败" value={job.failedCount} /></div>
      {job.error ? <InlineError message={job.error} /> : null}
      <div className="lux-emby-reports">
        <div className="lux-emby-report-tabs" role="tablist" aria-label="迁移报告">
          <ReportTabButton active={tab === "users"} onClick={() => onTabChange("users")}>用户关联</ReportTabButton>
          <ReportTabButton active={tab === "matches"} onClick={() => onTabChange("matches")}>媒体匹配</ReportTabButton>
          <ReportTabButton active={tab === "imports"} onClick={() => onTabChange("imports")}>导入结果</ReportTabButton>
        </div>
        {report.isPending ? <LoadingState label="正在读取报告…" /> : report.error ? <InlineError message={report.error.message} /> : <ReportContent tab={tab} data={report.data} page={reportPageFrom(report.data)} onPageChange={onPageChange} />}
      </div>
    </section>
  );
}

function ReportContent({ tab, data, page, onPageChange }: { tab: ReportTab; data: unknown; page: number; onPageChange: (page: number) => void }) {
  const entries = tab === "users" ? ((data as { users?: AdminEmbyMigrationUserLink[] })?.users ?? []) : tab === "matches" ? ((data as { matches?: AdminEmbyMigrationMatch[] })?.matches ?? []) : ((data as { imports?: AdminEmbyMigrationImport[] })?.imports ?? []);
  const response = data as { page?: number; pageSize?: number };
  const currentPage = response.page ?? page;
  const hasNext = entries.length >= (response.pageSize ?? pageSize);
  if (!entries.length) return <EmptyState label="此报告暂无记录" detail="任务运行后，这里会显示可核对的迁移结果。" />;
  return <>
    <div className="lux-emby-report-table">{tab === "users" ? entries.map((entry, index) => <UserReportRow key={`${entry.jobId}-${index}`} entry={entry as AdminEmbyMigrationUserLink} />) : tab === "matches" ? entries.map((entry, index) => <MatchReportRow key={`${entry.jobId}-${index}`} entry={entry as AdminEmbyMigrationMatch} />) : entries.map((entry, index) => <ImportReportRow key={`${entry.jobId}-${index}`} entry={entry as AdminEmbyMigrationImport} />)}</div>
    <div className="lux-emby-pagination"><button className="lux-button lux-button-secondary" type="button" disabled={currentPage <= 1} onClick={() => onPageChange(currentPage - 1)}>上一页</button><span>第 {currentPage} 页</span><button className="lux-button lux-button-secondary" type="button" disabled={!hasNext} onClick={() => onPageChange(currentPage + 1)}>下一页</button></div>
  </>;
}

function UserReportRow({ entry }: { entry: AdminEmbyMigrationUserLink }) { return <div className="lux-emby-report-row"><div><strong>{entry.embyUsername}</strong><small>Emby ID：{entry.embyUserId}</small></div><ReportStatus status={entry.status} /><span>{entry.luxUserId ? `Lux 用户：${entry.luxUserId}` : entry.error ?? "未关联"}</span></div>; }
function MatchReportRow({ entry }: { entry: AdminEmbyMigrationMatch }) { return <div className="lux-emby-report-row"><div><strong>{entry.detail.title ? String(entry.detail.title) : entry.embyItemId}</strong><small>{entry.embyItemType} · {entry.matchMethod}{entry.confidence == null ? "" : ` · ${entry.confidence}%`}</small></div><ReportStatus status={entry.status} /><span>{entry.luxItemId ? `Lux 媒体：${entry.luxItemId}` : safeDetail(entry.detail)}</span></div>; }
function ImportReportRow({ entry }: { entry: AdminEmbyMigrationImport }) { return <div className="lux-emby-report-row"><div><strong>{entry.embyItemId}</strong><small>Emby 用户：{entry.embyUserId}</small></div><ReportStatus status={entry.status} /><span>{entry.error ?? "状态已写入 Lux"}</span></div>; }

function ConnectionResult({ connection }: { connection: Awaited<ReturnType<typeof api.testAdminEmbyMigration>> }) { return <div className="lux-emby-connection-result"><CheckCircle2 size={18} /><div><strong>{connection.serverName || "Emby 服务器"}</strong><span>{[connection.productName, connection.version, connection.serverId ? `Server ID：${connection.serverId}` : null].filter(Boolean).join(" · ")}</span></div></div>; }
function CapabilityNotice({ capability }: { capability?: string }) { if (!capability) return null; const eventHistory = capability === "EVENT_HISTORY"; return <section className={`lux-emby-capability ${eventHistory ? "is-ok" : "is-warning"}`}><div className="lux-emby-capability-icon">{eventHistory ? <CheckCircle2 size={19} /> : <AlertTriangle size={19} />}</div><div><strong>{eventHistory ? "Emby 历史事件可用" : "完整历史播放时间线不可用"}</strong><p>{eventHistory ? "将按用户和媒体导入可获得的播放事件及进度。" : "Emby API 当前只能提供每个用户-媒体的聚合状态：已看、播放位置、播放次数、最近播放时间和收藏。旧的逐次播放事件（例如每次看到第几集的几分几秒）无法恢复。"}</p></div><CircleHelp size={17} aria-label="能力说明" /></section>; }
function JobSummary({ job, selected, onSelect }: { job: AdminEmbyMigrationJob; selected: boolean; onSelect: () => void }) { return <button className={`lux-emby-job-summary ${selected ? "is-selected" : ""}`} type="button" onClick={onSelect}><span><strong>{job.sourceLabel}</strong><small>{job.dryRun ? "预览" : "正式迁移"} · {phaseLabel(job.phase)}</small></span><span><ReportStatus status={job.status} /><small>{job.processedCount} / {job.totalCount}</small></span></button>; }
function ReportTabButton({ active, children, onClick }: { active: boolean; children: string; onClick: () => void }) { return <button className={active ? "is-active" : ""} type="button" role="tab" aria-selected={active} onClick={onClick}>{children}</button>; }
function PanelHeading({ headingId, icon, title, description }: { headingId: string; icon: ReactNode; title: string; description: string }) { return <div className="lux-admin-panel-heading"><div><span className="lux-admin-eyebrow">Emby → Lux</span><h2 id={headingId}>{title}</h2><p>{description}</p></div><span className="lux-emby-panel-icon">{icon}</span></div>; }
function Stat({ label, value }: { label: string; value: number }) { return <div><small>{label}</small><strong>{value.toLocaleString()}</strong></div>; }
function ReportStatus({ status }: { status: string }) { return <span className={`lux-emby-report-status is-${status.toLowerCase()}`}>{statusLabel(status)}</span>; }
function InlineError({ message }: { message: string }) { return <p className="lux-error-copy lux-emby-inline-error"><AlertTriangle size={15} /> {message}</p>; }
function LoadingState({ label }: { label: string }) { return <div className="lux-emby-state"><LoaderCircle size={18} className="lux-spin" /> <span>{label}</span></div>; }
function EmptyState({ label, detail }: { label: string; detail: string }) { return <div className="lux-emby-empty"><strong>{label}</strong><span>{detail}</span></div>; }
function isActiveJob(job?: AdminEmbyMigrationJob | null) { return Boolean(job && (job.status === "PENDING" || job.status === "RUNNING")); }
function statusLabel(status: string) { return ({ PENDING: "等待中", RUNNING: "进行中", COMPLETED: "已完成", CANCELLED: "已取消", FAILED: "失败" } as Record<string, string>)[status] ?? status; }
function phaseLabel(phase: string) { return ({ TESTING: "连接测试", USERS: "读取用户", ITEMS: "匹配媒体", IMPORTING: "导入状态", FINALIZING: "收尾" } as Record<string, string>)[phase] ?? phase; }
function safeDetail(detail: Record<string, unknown>) { const copy = Object.fromEntries(Object.entries(detail).filter(([key]) => !key.toLowerCase().includes("url") && !key.toLowerCase().includes("token"))); return JSON.stringify(copy); }
function reportPageFrom(data: unknown) { return (data as { page?: number } | undefined)?.page ?? 1; }
