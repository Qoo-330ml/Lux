import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, CloudDownload, LoaderCircle, RefreshCw, Save, ShieldCheck, XCircle } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminPlugin, AdminEmbyMigrationJob } from "../../lib/api/types";
import { EmbyMigrationReports, type ReportTab, useMigrationReport } from "./EmbyMigrationReports";

type MergePolicy = "MERGE" | "OVERWRITE" | "SKIP";
type ConnectionInfo = Awaited<ReturnType<typeof api.testAdminEmbyMigration>>;

const reportTabs: ReportTab[] = ["users", "matches", "imports", "personFavorites"];

export function EmbyMigrationPluginConfig({ plugin }: { plugin: AdminPlugin }) {
  const queryClient = useQueryClient();
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [apiKeyDirty, setApiKeyDirty] = useState(false);
  const [allowPrivateNetwork, setAllowPrivateNetwork] = useState(false);
  const [connection, setConnection] = useState<ConnectionInfo | null>(null);
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [connectionDirty, setConnectionDirty] = useState(false);
  const baseUrlField = plugin.configFields.find((field) => field.key === "baseUrl");
  const apiKeyField = plugin.configFields.find((field) => field.key === "apiKey");
  const privateNetworkField = plugin.configFields.find((field) => field.key === "allowPrivateNetwork");

  const clearConnection = () => {
    setConnection(null);
    setConnectionError(null);
  };
  const markConnectionDirty = () => {
    clearConnection();
    setConnectionDirty(true);
  };
  const save = useMutation({
    mutationFn: () => api.updateAdminPluginConfig(plugin.id, {
      baseUrl: baseUrl.trim(),
      ...(apiKeyDirty ? { apiKey } : {}),
      allowPrivateNetwork,
    }),
    onSuccess: () => {
      setApiKey("");
      setApiKeyDirty(false);
      clearConnection();
      setConnectionDirty(false);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
    },
  });

  useEffect(() => {
    const values = plugin.configValues ?? {};
    setBaseUrl(typeof values.baseUrl === "string" ? values.baseUrl : "");
    setAllowPrivateNetwork(values.allowPrivateNetwork === true);
    setApiKey("");
    setApiKeyDirty(false);
    clearConnection();
    setConnectionDirty(false);
  }, [plugin.configValues]);

  return (
    <div className="lux-emby-migration-plugin-config">
      <div className="lux-emby-migration-intro">
        <div>
          <span className="lux-admin-eyebrow">Emby → Lux</span>
          <p>保存并测试连接，先预览匹配结果，再执行导入。</p>
        </div>
        <span className="lux-emby-flow-label">3 步完成</span>
      </div>

      <section className="lux-emby-migration-section" aria-labelledby="emby-plugin-settings-heading">
        <StepHeading number="1" headingId="emby-plugin-settings-heading" title="连接 Emby" description="连接信息只保存在这个插件中。" icon={<ShieldCheck size={18} />} />
        <form className="lux-emby-plugin-settings-form" autoComplete="off" onSubmit={(event) => { event.preventDefault(); save.mutate(); }}>
          <div className="lux-emby-connection-fields">
            {baseUrlField ? <label htmlFor="emby-plugin-base-url">{baseUrlField.label}<input id="emby-plugin-base-url" type="url" required={baseUrlField.required} value={baseUrl} onChange={(event) => { setBaseUrl(event.target.value); markConnectionDirty(); }} placeholder="http://emby.local:8096" autoComplete="url" /><small>{baseUrlField.description}</small></label> : null}
            {apiKeyField ? <label htmlFor="emby-plugin-api-key">{apiKeyField.label}<input id="emby-plugin-api-key" type="password" required={apiKeyField.required && !plugin.configured} value={apiKey} onChange={(event) => { setApiKey(event.target.value); setApiKeyDirty(true); markConnectionDirty(); }} placeholder="留空保留已保存的 API Key" autoComplete="new-password" /><small>{apiKeyField.description}</small></label> : null}
          </div>
          {privateNetworkField ? <label className="lux-admin-toggle lux-emby-private-toggle"><input type="checkbox" checked={allowPrivateNetwork} onChange={(event) => { setAllowPrivateNetwork(event.target.checked); markConnectionDirty(); }} /><span><strong>{privateNetworkField.label}</strong><small>{privateNetworkField.description}</small></span></label> : null}
          <div className="lux-emby-plugin-settings-actions">
            <button className="lux-button lux-button-secondary" type="submit" disabled={save.isPending}><Save size={15} /> {save.isPending ? "保存中…" : "保存设置"}</button>
            <TestConnectionButton onStart={clearConnection} onSuccess={(result) => { setConnectionError(null); setConnection(result); }} onError={setConnectionError} disabled={save.isPending || connectionDirty} />
          </div>
          {connectionDirty ? <span className="lux-emby-unsaved-hint">保存更改后才能测试连接</span> : null}
          {save.error ? <InlineError message={save.error.message} /> : null}
        </form>
        {connectionError ? <InlineError message={connectionError} /> : null}
        {connection ? <ConnectionResult connection={connection} /> : null}
      </section>

      <MigrationWorkspace connection={connection} />
    </div>
  );
}

function TestConnectionButton({ onStart, onSuccess, onError, disabled }: { onStart: () => void; onSuccess: (connection: ConnectionInfo) => void; onError: (message: string) => void; disabled: boolean }) {
  const testConnection = useMutation({
    mutationFn: () => api.testAdminEmbyMigration(),
    onSuccess,
    onError: (error) => onError(error instanceof Error ? error.message : "连接测试失败"),
  });
  return <button className="lux-button lux-button-primary" type="button" aria-label="测试 Emby 连接" disabled={disabled || testConnection.isPending} onClick={() => { onStart(); testConnection.mutate(); }}><RefreshCw size={15} className={testConnection.isPending ? "lux-spin" : undefined} /> {testConnection.isPending ? "测试中…" : "测试连接"}</button>;
}

function MigrationWorkspace({ connection }: { connection: ConnectionInfo | null }) {
  const queryClient = useQueryClient();
  const [mergePolicy, setMergePolicy] = useState<MergePolicy>("MERGE");
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);
  const [tab, setTab] = useState<ReportTab>("users");
  const [reportPage, setReportPage] = useState<Record<ReportTab, number>>(() => initialReportPages());

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
  const previewReady = job?.dryRun === true && job.status === "COMPLETED";
  const active = isActiveJob(job);
  const canCreate = Boolean(connection && !active);

  useEffect(() => {
    if (!selectedJobId && jobs.data?.jobs?.[0]) selectJob(jobs.data.jobs[0].id);
  }, [jobs.data?.jobs, selectedJobId]);

  const createJob = useMutation({
    mutationFn: (dryRun: boolean) => api.createAdminEmbyMigration({ dryRun, mergePolicy }),
    onSuccess: ({ job: created }) => {
      selectJob(created.id);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminEmbyMigrations() });
    },
  });
  const cancelJob = useMutation({
    mutationFn: (jobId: string) => api.cancelAdminEmbyMigration(jobId),
    onSuccess: (_, jobId) => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminEmbyMigration(jobId) });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminEmbyMigrations() });
    },
  });
  const retryJob = useMutation({
    mutationFn: (jobId: string) => api.retryAdminEmbyMigration(jobId),
    onSuccess: (_, jobId) => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminEmbyMigration(jobId) });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminEmbyMigrations() });
    },
  });
  function selectJob(jobId: string) {
    setSelectedJobId(jobId);
    setTab("users");
    setReportPage(initialReportPages());
  }

  const actionIsPreview = !previewReady;
  const actionDisabled = !canCreate || createJob.isPending;
  const actionLabel = createJob.isPending
    ? "创建中…"
    : active
      ? "迁移进行中"
      : previewReady
        ? "执行迁移"
        : connection
          ? "生成预览"
          : "先测试连接";

  return (
    <>
      <section className="lux-emby-migration-section" aria-labelledby="emby-migration-action-heading">
        <StepHeading number="2" headingId="emby-migration-action-heading" title="迁移数据" description={previewReady ? "预览已完成，确认匹配结果后执行导入。" : "默认先生成预览，不会创建用户或修改播放状态。"} icon={<CloudDownload size={18} />} />
        <div className="lux-emby-migration-action">
          <div>
            <strong>{previewReady ? "预览已完成" : "先从预览开始"}</strong>
            <span>{previewReady ? "确认报告无误后，可以执行正式迁移。" : "默认合并已有播放状态。"}</span>
          </div>
          <button className="lux-button lux-button-primary" type="button" aria-label={previewReady ? "执行 Emby 迁移" : "开始 Emby 迁移"} disabled={actionDisabled} onClick={() => createJob.mutate(actionIsPreview)}>
            {previewReady ? <CloudDownload size={15} /> : <RefreshCw size={15} />}
            {actionLabel}
          </button>
        </div>
        <details className="lux-emby-advanced-options">
          <summary>高级选项 <span>默认：合并</span></summary>
          <label>
            <span>已有播放状态</span>
            <select value={mergePolicy} onChange={(event) => setMergePolicy(event.target.value as MergePolicy)}>
              <option value="MERGE">合并：保留两边更完整的状态</option>
              <option value="OVERWRITE">覆盖：使用 Emby 状态</option>
              <option value="SKIP">跳过：不修改已有状态</option>
            </select>
          </label>
        </details>
        {createJob.error ? <InlineError message={createJob.error.message} /> : null}
        {createJob.data ? <p className="lux-emby-migration-success" role="status"><CheckCircle2 size={15} /> {createJob.data.job.dryRun ? "预览任务已创建" : "正式迁移任务已创建"}</p> : null}
      </section>

      <section className="lux-emby-migration-section" aria-labelledby="emby-migration-jobs-heading">
        <StepHeading number="3" headingId="emby-migration-jobs-heading" title="任务与结果" description="任务会自动保存，运行中每 5 秒更新一次。" icon={<RefreshCw size={18} />} />
        {jobs.isPending ? <LoadingState label="正在读取任务…" /> : jobs.error ? <InlineError message={jobs.error.message} /> : jobs.data?.jobs?.length ? (
          <div className="lux-emby-job-list">
            {jobs.data.jobs.map((candidate) => <JobSummary key={candidate.id} job={candidate} selected={candidate.id === selectedJobId} onSelect={() => selectJob(candidate.id)} />)}
          </div>
        ) : <EmptyState label="还没有任务" detail="测试连接后，点击“生成预览”开始。" />}
      </section>

      {job ? <MigrationDetails key={job.id} job={job} tab={tab} page={reportPage[tab]} onTabChange={(nextTab) => { setTab(nextTab); setReportPage((current) => ({ ...current, [nextTab]: 1 })); }} onPageChange={(page) => setReportPage((current) => ({ ...current, [tab]: page }))} onCancel={() => cancelJob.mutate(job.id)} onRetry={() => retryJob.mutate(job.id)} actionPending={cancelJob.isPending || retryJob.isPending} /> : null}
    </>
  );
}

function MigrationDetails({ job, tab, page, onTabChange, onPageChange, onCancel, onRetry, actionPending }: { job: AdminEmbyMigrationJob; tab: ReportTab; page: number; onTabChange: (tab: ReportTab) => void; onPageChange: (page: number) => void; onCancel: () => void; onRetry: () => void; actionPending: boolean }) {
  const progress = job.totalCount > 0 ? Math.min(100, Math.round((job.processedCount / job.totalCount) * 100)) : 0;
  const active = isActiveJob(job);
  const [reportsOpen, setReportsOpen] = useState(false);
  const report = useMigrationReport(job.id, tab, page, reportsOpen);
  return (
    <section className="lux-emby-migration-section lux-emby-migration-details" aria-labelledby="emby-migration-detail-heading">
      <div className="lux-emby-detail-heading">
        <div><span className="lux-admin-eyebrow">当前任务</span><h3 id="emby-migration-detail-heading">{job.sourceLabel}</h3><p>{statusLabel(job.status)} · {phaseLabel(job.phase)} · {job.dryRun ? "预览" : "正式迁移"}</p></div>
        <div className="lux-emby-job-actions">
          {active ? <button className="lux-button lux-button-secondary" type="button" disabled={actionPending || job.cancelRequested} onClick={onCancel}><XCircle size={14} /> {job.cancelRequested ? "取消中…" : "取消"}</button> : null}
          {job.status === "FAILED" || job.status === "CANCELLED" ? <button className="lux-button lux-button-secondary" type="button" disabled={actionPending} onClick={onRetry}><RefreshCw size={14} /> 重试</button> : null}
        </div>
      </div>
      <div className="lux-emby-job-progress" aria-label={`迁移进度 ${progress}%`}>
        <div><strong>{progress}%</strong><span>{job.processedCount.toLocaleString()} / {job.totalCount.toLocaleString()}</span></div>
        <div className="lux-emby-progress-track"><span style={{ width: `${progress}%` }} /></div>
      </div>
      <div className="lux-emby-job-stats"><Stat label="已匹配" value={job.matchedCount} /><Stat label="已跳过" value={job.skippedCount} /><Stat label="失败" value={job.failedCount} /></div>
      {job.error ? <InlineError message={job.error} /> : null}
      <details className="lux-emby-reports" open={reportsOpen} onToggle={(event) => setReportsOpen(event.currentTarget.open)}>
        <summary>查看迁移报告 <span>用户、媒体、导入、人物收藏</span></summary>
        {reportsOpen ? <EmbyMigrationReports jobId={job.id} tab={tab} report={report} onTabChange={onTabChange} onPageChange={onPageChange} /> : null}
      </details>
    </section>
  );
}

function ConnectionResult({ connection }: { connection: ConnectionInfo }) {
  const eventHistory = connection.historyCapability === "EVENT_HISTORY";
  return <div className={`lux-emby-connection-result ${eventHistory ? "is-complete" : "is-limited"}`} role="status"><CheckCircle2 size={17} /><div><strong>{connection.serverName || "Emby 服务器"}</strong><span>{[connection.productName, connection.version].filter(Boolean).join(" · ")}</span></div><small>{eventHistory ? "完整历史时间线可用" : "完整历史播放时间线不可用"}</small></div>;
}

function StepHeading({ number, headingId, title, description, icon }: { number: string; headingId: string; title: string; description: string; icon: React.ReactNode }) {
  return <div className="lux-emby-step-heading"><span className="lux-emby-step-number" aria-hidden="true">{number}</span><div><span className="lux-emby-step-caption">第 {number} 步</span><h3 id={headingId}>{title}</h3><p>{description}</p></div><span className="lux-emby-step-icon" aria-hidden="true">{icon}</span></div>;
}

function JobSummary({ job, selected, onSelect }: { job: AdminEmbyMigrationJob; selected: boolean; onSelect: () => void }) {
  return <button className={`lux-emby-job-summary ${selected ? "is-selected" : ""}`} type="button" aria-label={`查看迁移任务 ${job.sourceLabel}`} onClick={onSelect}><span><strong>{job.sourceLabel}</strong><small>{job.dryRun ? "预览" : "正式迁移"} · {phaseLabel(job.phase)}</small></span><span><ReportStatus status={job.status} /><small>{job.processedCount} / {job.totalCount}</small></span></button>;
}

function Stat({ label, value }: { label: string; value: number }) { return <div><small>{label}</small><strong>{value.toLocaleString()}</strong></div>; }
function ReportStatus({ status }: { status: string }) { return <span className={`lux-emby-report-status is-${status.toLowerCase()}`}>{statusLabel(status)}</span>; }
function InlineError({ message }: { message: string }) { return <p className="lux-error-copy lux-emby-inline-error" role="alert"><AlertTriangle size={15} /> {message}</p>; }
function LoadingState({ label }: { label: string }) { return <div className="lux-emby-state" role="status"><LoaderCircle size={17} className="lux-spin" /> <span>{label}</span></div>; }
function EmptyState({ label, detail }: { label: string; detail: string }) { return <div className="lux-emby-empty"><strong>{label}</strong><span>{detail}</span></div>; }
function initialReportPages(): Record<ReportTab, number> { return Object.fromEntries(reportTabs.map((tab) => [tab, 1])) as Record<ReportTab, number>; }
function isActiveJob(job?: AdminEmbyMigrationJob | null) { return Boolean(job && (job.status === "PENDING" || job.status === "RUNNING")); }
function statusLabel(status: string) { return ({ PENDING: "等待中", RUNNING: "进行中", COMPLETED: "已完成", CANCELLED: "已取消", FAILED: "失败" } as Record<string, string>)[status] ?? status; }
function phaseLabel(phase: string) { return ({ TESTING: "连接测试", USERS: "读取用户", ITEMS: "匹配媒体", IMPORTING: "导入状态", FINALIZING: "收尾" } as Record<string, string>)[phase] ?? phase; }
