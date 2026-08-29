import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, CloudDownload, LoaderCircle, RefreshCw, Save, ShieldCheck, XCircle } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminPlugin, AdminEmbyMigrationJob, AdminEmbyMigrationSourceUser } from "../../lib/api/types";
import { EmbyMigrationReports, type ReportTab, useMigrationReport } from "./EmbyMigrationReports";

type MergePolicy = "MERGE" | "OVERWRITE" | "SKIP";
type MigrationStep = 1 | 2 | 3;
type ConnectionInfo = Awaited<ReturnType<typeof api.testAdminEmbyMigration>>;

const reportTabs: ReportTab[] = ["users", "matches", "imports", "personFavorites"];
const stepDefinitions = [
  { number: 1, title: "连接 Emby" },
  { number: 2, title: "选择迁移范围" },
  { number: 3, title: "确认并开始" },
] as const;

export function EmbyMigrationPluginConfig({ plugin }: { plugin: AdminPlugin }) {
  const queryClient = useQueryClient();
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [apiKeyDirty, setApiKeyDirty] = useState(false);
  const [allowPrivateNetwork, setAllowPrivateNetwork] = useState(false);
  const [connection, setConnection] = useState<ConnectionInfo | null>(null);
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [connectionDirty, setConnectionDirty] = useState(false);
  const [currentStep, setCurrentStep] = useState<MigrationStep>(1);
  const [selectedUserIds, setSelectedUserIds] = useState<string[]>([]);
  const [mergePolicy, setMergePolicy] = useState<MergePolicy>("MERGE");
  const baseUrlField = plugin.configFields.find((field) => field.key === "baseUrl");
  const apiKeyField = plugin.configFields.find((field) => field.key === "apiKey");
  const privateNetworkField = plugin.configFields.find((field) => field.key === "allowPrivateNetwork");

  const clearConnection = () => {
    setConnection(null);
    setConnectionError(null);
  };
  const markConnectionDirty = () => {
    clearConnection();
    setSelectedUserIds([]);
    setConnectionDirty(true);
    setCurrentStep(1);
  };
  const saveAndTest = useMutation({
    mutationFn: async () => {
      await api.updateAdminPluginConfig(plugin.id, {
        baseUrl: baseUrl.trim(),
        ...(apiKeyDirty ? { apiKey } : {}),
        allowPrivateNetwork,
      });
      return api.testAdminEmbyMigration();
    },
    onMutate: () => setConnectionError(null),
    onSuccess: (result) => {
      setApiKey("");
      setApiKeyDirty(false);
      setConnection(result);
      setConnectionDirty(false);
      setCurrentStep(2);
    },
    onError: (error) => setConnectionError(error instanceof Error ? error.message : "连接测试失败"),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminPlugins });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminInstalledPlugins });
    },
  });

  useEffect(() => {
    if (connection) return;
    const values = plugin.configValues ?? {};
    setBaseUrl(typeof values.baseUrl === "string" ? values.baseUrl : "");
    setAllowPrivateNetwork(values.allowPrivateNetwork === true);
    setApiKey("");
    setApiKeyDirty(false);
    setConnectionDirty(false);
  }, [connection, plugin.configValues]);

  return (
    <div className="lux-emby-migration-plugin-config">
      <div className="lux-emby-migration-intro">
        <div>
          <span className="lux-admin-eyebrow">Emby → Lux</span>
          <p>按三个步骤完成迁移，只处理你明确选择的 Emby 用户。</p>
        </div>
        <span className="lux-emby-flow-label">3 步完成</span>
      </div>

      <MigrationStepper currentStep={currentStep} />

      {connection && currentStep > 1 ? <ConnectionResult connection={connection} compact /> : null}

      <div className="lux-emby-step-panel" data-testid="emby-migration-step-panel" data-step={currentStep}>
        {currentStep === 1 ? (
          <section aria-labelledby="emby-plugin-settings-heading">
            <StepHeading number="1" headingId="emby-plugin-settings-heading" title="连接 Emby" description="先保存并验证来源服务器，成功后才能选择迁移范围。" icon={<ShieldCheck size={18} />} />
            <form className="lux-emby-plugin-settings-form" autoComplete="off" onSubmit={(event) => { event.preventDefault(); saveAndTest.mutate(); }}>
              <div className="lux-emby-connection-fields">
                {baseUrlField ? <label htmlFor="emby-plugin-base-url">{baseUrlField.label}<input id="emby-plugin-base-url" type="url" required={baseUrlField.required} value={baseUrl} onChange={(event) => { setBaseUrl(event.target.value); markConnectionDirty(); }} placeholder="http://emby.local:8096" autoComplete="url" /><small>{baseUrlField.description}</small></label> : null}
                {apiKeyField ? <label htmlFor="emby-plugin-api-key">{apiKeyField.label}<input id="emby-plugin-api-key" type="password" required={apiKeyField.required && !plugin.configured} value={apiKey} onChange={(event) => { setApiKey(event.target.value); setApiKeyDirty(true); markConnectionDirty(); }} placeholder="留空保留已保存的 API Key" autoComplete="new-password" /><small>{apiKeyField.description}</small></label> : null}
              </div>
              {privateNetworkField ? <label className="lux-admin-toggle lux-emby-private-toggle"><input type="checkbox" checked={allowPrivateNetwork} onChange={(event) => { setAllowPrivateNetwork(event.target.checked); markConnectionDirty(); }} /><span><strong>{privateNetworkField.label}</strong><small>{privateNetworkField.description}</small></span></label> : null}
              <div className="lux-emby-wizard-actions">
                <span className="lux-emby-wizard-step-note">第 1 步，共 3 步</span>
                <button className="lux-button lux-button-primary" type="submit" aria-label="保存并测试 Emby 连接" disabled={saveAndTest.isPending}>
                  <Save size={15} /> {saveAndTest.isPending ? "验证中…" : "保存并测试连接"}
                </button>
              </div>
              {connectionDirty ? <span className="lux-emby-unsaved-hint">修改后需要重新保存并测试连接</span> : null}
              {connectionError ? <InlineError message={connectionError} /> : null}
            </form>
            {connection ? <ConnectionResult connection={connection} /> : null}
          </section>
        ) : (
          <MigrationWorkspace connection={connection} currentStep={currentStep} onStepChange={setCurrentStep} selectedUserIds={selectedUserIds} onSelectedUserIdsChange={setSelectedUserIds} mergePolicy={mergePolicy} onMergePolicyChange={setMergePolicy} />
        )}
      </div>
    </div>
  );
}

function MigrationStepper({ currentStep }: { currentStep: MigrationStep }) {
  return (
    <nav className="lux-emby-stepper" aria-label="Emby 迁移步骤">
      <ol>
        {stepDefinitions.map((step, index) => (
          <li key={step.number} className={currentStep === step.number ? "is-active" : currentStep > step.number ? "is-complete" : ""} aria-current={currentStep === step.number ? "step" : undefined}>
            <span className="lux-emby-stepper-marker" aria-hidden="true">{currentStep > step.number ? <CheckCircle2 size={14} /> : step.number}</span>
            <span className="lux-emby-stepper-copy"><small>第 {step.number} 步</small><strong>{step.title}</strong></span>
            {index < stepDefinitions.length - 1 ? <span className="lux-emby-stepper-line" aria-hidden="true" /> : null}
          </li>
        ))}
      </ol>
    </nav>
  );
}

function MigrationWorkspace({ connection, currentStep, onStepChange, selectedUserIds, onSelectedUserIdsChange, mergePolicy, onMergePolicyChange }: { connection: ConnectionInfo | null; currentStep: MigrationStep; onStepChange: (step: MigrationStep) => void; selectedUserIds: string[]; onSelectedUserIdsChange: (ids: string[]) => void; mergePolicy: MergePolicy; onMergePolicyChange: (policy: MergePolicy) => void }) {
  const queryClient = useQueryClient();
  const [sourceUserPage, setSourceUserPage] = useState(1);
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);
  const [tab, setTab] = useState<ReportTab>("users");
  const [reportPage, setReportPage] = useState<Record<ReportTab, number>>(() => initialReportPages());
  const [historyOpen, setHistoryOpen] = useState(false);

  const sourceUsers = useQuery({
    queryKey: queryKeys.adminEmbyMigrationSourceUsers(sourceUserPage),
    queryFn: () => api.adminEmbyMigrationSourceUsers(sourceUserPage),
    enabled: Boolean(connection && currentStep === 2),
    staleTime: 5 * 60_000,
    retry: false,
  });
  const users = sourceUsers.data?.users ?? [];
  const sourceUserPageSize = sourceUsers.data?.pageSize ?? 100;
  const sourceUserTotal = sourceUsers.data?.total ?? 0;
  const sourceUserPages = Math.max(1, Math.ceil(sourceUserTotal / sourceUserPageSize));
  const allVisibleUsersSelected = users.length > 0 && users.every((user) => selectedUserIds.includes(user.id));

  const jobs = useQuery({
    queryKey: queryKeys.adminEmbyMigrations(),
    queryFn: () => api.adminEmbyMigrations(),
    enabled: historyOpen && Boolean(connection),
    refetchInterval: historyOpen ? 5_000 : false,
  });
  const selectedFromList = jobs.data?.jobs?.find((job) => job.id === selectedJobId) ?? null;
  const detail = useQuery({
    queryKey: queryKeys.adminEmbyMigration(selectedJobId ?? "none"),
    queryFn: () => api.adminEmbyMigration(selectedJobId!),
    enabled: Boolean(selectedJobId),
    refetchInterval: (query) => isActiveJob(query.state.data?.job) ? 5_000 : false,
  });
  const job = detail.data?.job ?? selectedFromList;
  const active = isActiveJob(job);
  const canCreate = Boolean(connection && selectedUserIds.length > 0 && !active);

  useEffect(() => {
    setSourceUserPage(1);
    setSelectedJobId(null);
  }, [connection]);

  const createJob = useMutation({
    mutationFn: (dryRun: boolean) => api.createAdminEmbyMigration({
      dryRun,
      mergePolicy,
      embyUserIds: selectedUserIds,
    }),
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

  const toggleUser = (user: AdminEmbyMigrationSourceUser) => {
    onSelectedUserIdsChange(selectedUserIds.includes(user.id)
      ? selectedUserIds.filter((id) => id !== user.id)
      : [...selectedUserIds, user.id]);
  };

  return (
    <>
      {currentStep === 2 ? (
        <section aria-labelledby="emby-migration-users-heading">
          <StepHeading number="2" headingId="emby-migration-users-heading" title="选择迁移范围" description="选择要迁移的 Emby 用户；只会读取和迁移勾选用户的状态、收藏和权限。" icon={<ShieldCheck size={18} />} />
          {!connection ? <EmptyState label="请返回第一步连接 Emby" detail="连接成功后才能选择要迁移的用户。" /> : sourceUsers.isPending ? <LoadingState label="正在读取 Emby 用户…" /> : sourceUsers.error ? <InlineError message={sourceUsers.error.message} /> : (
            <>
              <div className="lux-emby-user-selection-toolbar">
                <span>已选 {selectedUserIds.length} / {sourceUserTotal} 位用户</span>
                <div>
                  <button className="lux-button lux-button-secondary" type="button" disabled={users.length === 0 || allVisibleUsersSelected} onClick={() => onSelectedUserIdsChange(Array.from(new Set([...selectedUserIds, ...users.map((user) => user.id)])))}>全选当前页</button>
                  <button className="lux-button lux-button-secondary" type="button" disabled={selectedUserIds.length === 0} onClick={() => onSelectedUserIdsChange([])}>清空</button>
                </div>
              </div>
              <div className="lux-emby-user-selection" role="group" aria-labelledby="emby-migration-users-heading">
                {users.map((user) => <label key={user.id} className="lux-emby-user-option">
                  <input type="checkbox" checked={selectedUserIds.includes(user.id)} onChange={() => toggleUser(user)} aria-label={`选择 Emby 用户 ${user.name}`} />
                  <span><strong>{user.name}</strong><small>{user.isDisabled ? "已禁用" : "可用"}{user.isAdministrator ? " · 管理员" : ""}</small></span>
                </label>)}
                {users.length === 0 ? <EmptyState label="没有可迁移的用户" detail="Emby 没有返回用户列表。" /> : null}
              </div>
              {sourceUserPages > 1 ? <div className="lux-emby-pagination" aria-label="Emby 用户分页">
                <button className="lux-button lux-button-secondary" type="button" disabled={sourceUserPage <= 1} onClick={() => setSourceUserPage((page) => page - 1)}>上一页</button>
                <span>第 {sourceUserPage} / {sourceUserPages} 页</span>
                <button className="lux-button lux-button-secondary" type="button" disabled={sourceUserPage >= sourceUserPages} onClick={() => setSourceUserPage((page) => page + 1)}>下一页</button>
              </div> : null}
              <div className="lux-emby-migration-scope-note"><CheckCircle2 size={15} /><span>将迁移用户资料、媒体库权限、已看状态、播放进度、播放次数、最近播放时间、收藏和人物收藏。</span></div>
            </>
          )}
          <div className="lux-emby-wizard-actions">
            <button className="lux-button lux-button-secondary" type="button" onClick={() => onStepChange(1)}>上一步</button>
            <button className="lux-button lux-button-primary" type="button" aria-label="下一步：确认迁移" disabled={selectedUserIds.length === 0} onClick={() => onStepChange(3)}>下一步：确认迁移</button>
          </div>
        </section>
      ) : (
        <section aria-labelledby="emby-migration-action-heading">
          <StepHeading number="3" headingId="emby-migration-action-heading" title="确认并开始" description="确认迁移范围和已有状态处理策略，然后启动后台任务。" icon={<CloudDownload size={18} />} />
          <div className="lux-emby-confirm-summary" aria-label="迁移摘要">
            <div><small>来源</small><strong>{connection?.serverName || "Emby 服务器"}</strong><span>{[connection?.productName, connection?.version].filter(Boolean).join(" · ") || "已验证连接"}</span></div>
            <div><small>迁移用户</small><strong>{selectedUserIds.length} 位</strong><span>不会迁移未勾选的用户</span></div>
            <div><small>历史能力</small><strong>{connection?.historyCapability === "EVENT_HISTORY" ? "完整历史" : "条目状态"}</strong><span>{connection?.historyCapability === "EVENT_HISTORY" ? "可导入原始播放时间线" : "不生成虚假的历史时间线"}</span></div>
          </div>
          <fieldset className="lux-emby-merge-policy">
            <legend>已有播放状态</legend>
            <div className="lux-emby-merge-options">
              <MergePolicyOption value="MERGE" selected={mergePolicy} onChange={onMergePolicyChange} title="合并" detail="保留两边更完整的状态（推荐）" />
              <MergePolicyOption value="OVERWRITE" selected={mergePolicy} onChange={onMergePolicyChange} title="覆盖" detail="使用 Emby 的状态覆盖 Lux" />
              <MergePolicyOption value="SKIP" selected={mergePolicy} onChange={onMergePolicyChange} title="跳过" detail="不修改 Lux 已有状态" />
            </div>
          </fieldset>
          <div className="lux-emby-migration-action">
            <div><strong>{selectedUserIds.length > 0 ? `准备迁移 ${selectedUserIds.length} 位用户` : "请返回第二步选择用户"}</strong><span>迁移会在后台运行，可以在下方查看进度和报告。</span></div>
            <div className="lux-emby-migration-action-buttons">
              <button className="lux-button lux-button-secondary" type="button" aria-label="生成 Emby 迁移预览" disabled={!canCreate || createJob.isPending} onClick={() => createJob.mutate(true)}><RefreshCw size={15} /> 先生成预览</button>
              <button className="lux-button lux-button-primary" type="button" aria-label="开始 Emby 迁移" disabled={!canCreate || createJob.isPending} onClick={() => createJob.mutate(false)}><CloudDownload size={15} /> {createJob.isPending ? "创建中…" : active ? "迁移进行中" : "开始迁移"}</button>
            </div>
          </div>
          {createJob.error ? <InlineError message={createJob.error.message} /> : null}
          {createJob.data ? <p className="lux-emby-migration-success" role="status"><CheckCircle2 size={15} /> {createJob.data.job.dryRun ? "预览任务已创建，可在下方查看报告" : "迁移任务已创建"}</p> : null}
          <div className="lux-emby-wizard-actions">
            <button className="lux-button lux-button-secondary" type="button" onClick={() => onStepChange(2)}>上一步</button>
          </div>
        </section>
      )}

      <details className="lux-emby-migration-section lux-emby-history" open={historyOpen} onToggle={(event) => setHistoryOpen(event.currentTarget.open)}>
        <summary aria-labelledby="emby-migration-jobs-heading"><span className="lux-admin-eyebrow">可选</span><h3 id="emby-migration-jobs-heading">任务记录与报告</h3><span>需要时查看历史任务和详细结果</span></summary>
        {jobs.isPending ? <LoadingState label="正在读取任务…" /> : jobs.error ? <InlineError message={jobs.error.message} /> : jobs.data?.jobs?.length ? (
          <div className="lux-emby-job-list">
            {jobs.data.jobs.map((candidate) => <JobSummary key={candidate.id} job={candidate} selected={candidate.id === selectedJobId} onSelect={() => selectJob(candidate.id)} />)}
          </div>
        ) : <EmptyState label="还没有任务" detail="完成前三步后，迁移任务会出现在这里。" />}
      </details>

      {job ? <MigrationDetails key={job.id} job={job} tab={tab} page={reportPage[tab]} onTabChange={(nextTab) => { setTab(nextTab); setReportPage((current) => ({ ...current, [nextTab]: 1 })); }} onPageChange={(page) => setReportPage((current) => ({ ...current, [tab]: page }))} onCancel={() => cancelJob.mutate(job.id)} onRetry={() => retryJob.mutate(job.id)} actionPending={cancelJob.isPending || retryJob.isPending} /> : null}
    </>
  );
}

function MergePolicyOption({ value, selected, onChange, title, detail }: { value: MergePolicy; selected: MergePolicy; onChange: (value: MergePolicy) => void; title: string; detail: string }) {
  return <label className={`lux-emby-merge-option${selected === value ? " is-selected" : ""}`}><input type="radio" name="emby-merge-policy" value={value} checked={selected === value} onChange={() => onChange(value)} /><span><strong>{title}</strong><small>{detail}</small></span></label>;
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

function ConnectionResult({ connection, compact = false }: { connection: ConnectionInfo; compact?: boolean }) {
  const eventHistory = connection.historyCapability === "EVENT_HISTORY";
  return <div className={`lux-emby-connection-result ${eventHistory ? "is-complete" : "is-limited"}${compact ? " is-compact" : ""}`} role="status"><CheckCircle2 size={17} /><div><strong>{connection.serverName || "Emby 服务器"}</strong><span>{[connection.productName, connection.version].filter(Boolean).join(" · ")}</span></div><small>{eventHistory ? "完整历史时间线可用" : "仅迁移条目状态，历史时间线不可用"}</small></div>;
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
