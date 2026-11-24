import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowRight, CheckCircle2, ClipboardList, Pencil, Play, Plus, Save, Trash2, X } from "lucide-react";
import { useEffect, useMemo, useState, type FormEvent } from "react";
import { LuxSelect } from "../../components/LuxSelect";
import { LuxButton, LuxCheckbox, LuxDialog, LuxIconButton, LuxSwitch, LuxTextField, useLuxConfirm } from "../../components/ui";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminLibrary, AdminScheduledTaskPlan } from "../../lib/api/types";

export const SCHEDULE_TASK_TYPE_OPTIONS: Array<[string, string]> = [
  ["RECONCILIATION_SCAN", "全量校验媒体库"],
  ["METADATA_PARSE", "元数据刮削"],
  ["CHAPTER_DETECTION", "片头片尾检测"],
  ["AUTO_LIBRARY_COVER", "自动生成媒体库封面"],
];

const WEEKDAY_LABELS = ["周日", "周一", "周二", "周三", "周四", "周五", "周六", "周日"];

type CronDescription = {
  human: string;
  sub: string;
  localShort: string;
  valid: boolean;
};

/** Converts a local HH:MM (and optional weekday 0-6) to a 5-field UTC Cron string. */
function localTimeToUtcCron(localHour: number, localMinute: number, localDow?: number): string {
  const offsetMinutes = new Date().getTimezoneOffset(); // e.g., -480 for UTC+8
  const totalUtcMinutes = localHour * 60 + localMinute + offsetMinutes;
  const dayShift = Math.floor(totalUtcMinutes / 1440);
  const normalizedMinutes = ((totalUtcMinutes % 1440) + 1440) % 1440;
  const utcHour = Math.floor(normalizedMinutes / 60);
  const utcMinute = normalizedMinutes % 60;
  if (localDow === undefined) {
    return `${utcMinute} ${utcHour} * * *`;
  }
  const utcDow = (((localDow + dayShift) % 7) + 7) % 7;
  return `${utcMinute} ${utcHour} * * ${utcDow}`;
}

function buildCronPresets(allowEmpty: boolean): Array<{ label: string; cron: string }> {
  const presets = [
    { label: "每天 02:00 (本地)", cron: localTimeToUtcCron(2, 0) },
    { label: "每天 03:00 (本地)", cron: localTimeToUtcCron(3, 0) },
    { label: "每天 04:00 (本地)", cron: localTimeToUtcCron(4, 0) },
    { label: "每周日 04:00 (本地)", cron: localTimeToUtcCron(4, 0, 0) },
    { label: "每 6 小时", cron: "0 */6 * * *" },
  ];
  if (allowEmpty) {
    presets.push({ label: "仅手动触发", cron: "" });
  }
  return presets;
}

function parseCronInteger(text: string, minimum: number, maximum: number): number | null {
  if (!/^\d+$/.test(text)) return null;
  const value = Number(text);
  if (!Number.isInteger(value) || value < minimum || value > maximum) return null;
  return value;
}

function isValidCronField(field: string, minimum: number, maximum: number): boolean {
  if (!field) return false;
  for (const item of field.split(",")) {
    if (!item) return false;
    const slashIndex = item.indexOf("/");
    let range = item;
    let hasStep = false;
    if (slashIndex !== -1) {
      if (item.indexOf("/", slashIndex + 1) !== -1) return false;
      range = item.slice(0, slashIndex);
      const stepText = item.slice(slashIndex + 1);
      const step = parseCronInteger(stepText, 1, 255);
      if (step === null) return false;
      hasStep = true;
    }
    if (range === "*") continue;
    const dashIndex = range.indexOf("-");
    if (dashIndex !== -1) {
      if (range.indexOf("-", dashIndex + 1) !== -1) return false;
      const start = parseCronInteger(range.slice(0, dashIndex), minimum, maximum);
      const end = parseCronInteger(range.slice(dashIndex + 1), minimum, maximum);
      if (start === null || end === null || start > end) return false;
      continue;
    }
    if (hasStep) {
      if (parseCronInteger(range, minimum, maximum) === null) return false;
      continue;
    }
    if (parseCronInteger(range, minimum, maximum) === null) return false;
  }
  return true;
}

/** Formats a 5-field UTC Cron expression into local-time readable copy and validates field ranges. */
export function describeCronSchedule(cron?: string | null): CronDescription {
  const trimmed = cron?.trim() ?? "";
  if (!trimmed) {
    return {
      human: "仅手动触发",
      sub: "尚未配置定时 Cron",
      localShort: "仅手动",
      valid: true,
    };
  }
  const parts = trimmed.split(/\s+/);
  if (parts.length !== 5) {
    return {
      human: "Cron 格式需为五段式（分 时 日 月 周）",
      sub: `当前输入：${trimmed}`,
      localShort: trimmed,
      valid: false,
    };
  }
  const [minute, hour, dayOfMonth, month, dayOfWeek] = parts;
  if (
    !isValidCronField(minute, 0, 59)
    || !isValidCronField(hour, 0, 23)
    || !isValidCronField(dayOfMonth, 1, 31)
    || !isValidCronField(month, 1, 12)
    || !isValidCronField(dayOfWeek, 0, 7)
  ) {
    return {
      human: "Cron 字段格式或数值超出范围（分 0-59、时 0-23、日 1-31、月 1-12、周 0-7）",
      sub: `当前输入：${trimmed}`,
      localShort: trimmed,
      valid: false,
    };
  }
  if (minute === "0" && /^\*\/([1-9]\d*)$/.test(hour) && dayOfMonth === "*" && month === "*" && dayOfWeek === "*") {
    const interval = hour.match(/^\*\/(\d+)$/)?.[1] ?? "";
    return {
      human: `每 ${interval} 小时执行一次`,
      sub: `UTC Cron：${trimmed}`,
      localShort: `每 ${interval} 小时`,
      valid: true,
    };
  }
  if (/^\d{1,2}$/.test(minute) && /^\d{1,2}$/.test(hour) && dayOfMonth === "*" && month === "*") {
    const utcMinute = Number(minute);
    const utcHour = Number(hour);
    const offsetMinutes = new Date().getTimezoneOffset(); // UTC - local in minutes
    const localTotal = utcHour * 60 + utcMinute - offsetMinutes;
    const dayShift = Math.floor(localTotal / 1440);
    const normalizedLocal = ((localTotal % 1440) + 1440) % 1440;
    const localHour = Math.floor(normalizedLocal / 60);
    const localMinute = normalizedLocal % 60;
    const localTime = `${String(localHour).padStart(2, "0")}:${String(localMinute).padStart(2, "0")}`;
    const utcTime = `${String(utcHour).padStart(2, "0")}:${String(utcMinute).padStart(2, "0")}`;
    const sameOffset = offsetMinutes === 0;
    if (dayOfWeek === "*") {
      return {
        human: sameOffset ? `每天 ${localTime} (UTC)` : `每天 ${localTime} (本地)`,
        sub: sameOffset ? trimmed : `UTC ${utcTime} · ${trimmed}`,
        localShort: `每天 ${localTime}`,
        valid: true,
      };
    }
    if (/^[0-7]$/.test(dayOfWeek)) {
      const utcDow = Number(dayOfWeek) === 7 ? 0 : Number(dayOfWeek);
      const localDow = (((utcDow + dayShift) % 7) + 7) % 7;
      return {
        human: sameOffset ? `每${WEEKDAY_LABELS[localDow]} ${localTime} (UTC)` : `每${WEEKDAY_LABELS[localDow]} ${localTime} (本地)`,
        sub: sameOffset ? trimmed : `UTC 每${WEEKDAY_LABELS[utcDow]} ${utcTime} · ${trimmed}`,
        localShort: `每${WEEKDAY_LABELS[localDow]} ${localTime}`,
        valid: true,
      };
    }
  }
  return {
    human: `自定义定时（${trimmed}）`,
    sub: "按 UTC 时区执行",
    localShort: trimmed,
    valid: true,
  };
}

type RunFeedback = {
  planName: string;
  queuedCount: number;
  skippedLibraryNames: string[];
  isGlobal: boolean;
};

type DialogState =
  | { mode: "create"; taskType: string; initialLibraryIds: string[]; lockTaskType: boolean }
  | { mode: "edit"; plan: AdminScheduledTaskPlan };

type TaskGroupSummary = {
  taskType: string;
  label: string;
  description: string;
  sourceType?: string;
  pluginId?: string | null;
  scopeType: "GLOBAL" | "LIBRARY";
  visiblePlans: AdminScheduledTaskPlan[];
  allTaskPlans: AdminScheduledTaskPlan[];
  supportedLibraries: Array<{ id: string; name: string }>;
};

export function RegisteredPlansWorkbench({
  plans,
  allPlans,
  baselinePlans,
  libraries,
  page,
  pageSize,
  total,
  search,
  onSearchChange,
  onPageChange,
  onRefresh,
  onOpenRunsTab,
  taskLabel,
  pagination,
}: {
  plans: AdminScheduledTaskPlan[];
  allPlans: AdminScheduledTaskPlan[];
  baselinePlans: AdminScheduledTaskPlan[];
  libraries: AdminLibrary[];
  page: number;
  pageSize: number;
  total: number;
  search: string;
  onSearchChange: (value: string) => void;
  onPageChange: (page: number) => void;
  onRefresh: () => void;
  onOpenRunsTab: () => void;
  taskLabel: (taskType: string) => string;
  pagination: React.ReactNode;
}) {
  const [selectedTaskType, setSelectedTaskType] = useState<string>("");
  const [dialogState, setDialogState] = useState<DialogState | null>(null);
  const [runFeedback, setRunFeedback] = useState<RunFeedback | null>(null);
  const [emptiedPlanNotice, setEmptiedPlanNotice] = useState<{ id: string; name: string } | null>(null);
  const [assignError, setAssignError] = useState<string>("");

  const taskGroups = useMemo<TaskGroupSummary[]>(() => {
    const libraryNameMap = new Map(libraries.map((lib) => [lib.id, lib.name]));
    const visibleByType = new Map<string, AdminScheduledTaskPlan[]>();
    const allByType = new Map<string, AdminScheduledTaskPlan[]>();
    const baselineByType = new Map<string, AdminScheduledTaskPlan[]>();

    for (const plan of plans) {
      const list = visibleByType.get(plan.taskType) ?? [];
      list.push(plan);
      visibleByType.set(plan.taskType, list);
    }
    for (const plan of allPlans) {
      const list = allByType.get(plan.taskType) ?? [];
      list.push(plan);
      allByType.set(plan.taskType, list);
    }
    for (const plan of baselinePlans) {
      const list = baselineByType.get(plan.taskType) ?? [];
      list.push(plan);
      baselineByType.set(plan.taskType, list);
    }

    const groups: TaskGroupSummary[] = [];
    for (const [taskType, visiblePlans] of visibleByType.entries()) {
      const allTaskPlans = baselineByType.get(taskType) ?? allByType.get(taskType) ?? visiblePlans;
      const representative = visiblePlans[0] ?? allTaskPlans[0];
      const scopeType = representative?.scopeType === "GLOBAL" ? "GLOBAL" : "LIBRARY";
      const supportedMap = new Map<string, string>();
      for (const plan of allTaskPlans) {
        for (const lib of plan.libraries ?? []) {
          supportedMap.set(lib.id, libraryNameMap.get(lib.id) ?? lib.name);
        }
      }
      const supportedLibraries = [...supportedMap.entries()].map(([id, name]) => ({ id, name }));
      groups.push({
        taskType,
        label: taskGroupLabel(taskType, taskLabel),
        description: representative?.description || "由系统或插件注册的后台任务。",
        sourceType: representative?.sourceType,
        pluginId: representative?.pluginId,
        scopeType,
        visiblePlans,
        allTaskPlans,
        supportedLibraries,
      });
    }
    return groups;
  }, [allPlans, baselinePlans, libraries, plans, taskLabel]);

  const creatableTaskTypeOptions = useMemo(() => {
    const registeredLibraryTaskTypes = new Set<string>();
    for (const plan of baselinePlans) {
      if (plan.scopeType !== "GLOBAL" && (plan.libraries?.length ?? plan.libraryCount ?? 0) > 0) {
        registeredLibraryTaskTypes.add(plan.taskType);
      }
    }
    return SCHEDULE_TASK_TYPE_OPTIONS.filter(([type]) => registeredLibraryTaskTypes.has(type));
  }, [baselinePlans]);

  const activeGroup = useMemo(() => {
    if (taskGroups.length === 0) return null;
    return taskGroups.find((group) => group.taskType === selectedTaskType) ?? taskGroups[0];
  }, [selectedTaskType, taskGroups]);

  useEffect(() => {
    if (activeGroup && activeGroup.taskType !== selectedTaskType) {
      setSelectedTaskType(activeGroup.taskType);
    }
  }, [activeGroup, selectedTaskType]);

  const libraryGroups = taskGroups.filter((group) => group.scopeType === "LIBRARY");
  const globalGroups = taskGroups.filter((group) => group.scopeType === "GLOBAL");

  const supportedIdsForActive = new Set(activeGroup?.supportedLibraries.map((lib) => lib.id) ?? []);
  const unsupportedLibraries = activeGroup && activeGroup.scopeType === "LIBRARY"
    ? libraries.filter((lib) => !supportedIdsForActive.has(lib.id))
    : [];

  const queryClient = useQueryClient();
  const quickAssign = useMutation({
    mutationFn: async ({ targetPlanId, libraryId, taskType }: { targetPlanId: string; libraryId: string; taskType: string }) => {
      const pool = baselinePlans.length >= allPlans.length ? baselinePlans : allPlans;
      const targetPlan = pool.find((plan) => plan.id === targetPlanId);
      if (!targetPlan) throw new Error("目标执行计划不存在");
      const sourcePlan = pool.find((plan) =>
        plan.taskType === taskType
        && plan.id !== targetPlanId
        && (plan.libraries ?? []).some((lib) => lib.id === libraryId),
      );
      const currentIds = (targetPlan.libraries ?? []).map((lib) => lib.id);
      const nextIds = currentIds.includes(libraryId) ? currentIds : [...currentIds, libraryId];
      await api.updateAdminScheduledTaskPlan(targetPlanId, { libraryIds: nextIds });
      if (sourcePlan && !sourcePlan.isDefault && (sourcePlan.libraries?.length ?? 0) === 1) {
        return { id: sourcePlan.id, name: sourcePlan.name || taskGroupLabel(sourcePlan.taskType, taskLabel) };
      }
      return null;
    },
    onSuccess: (emptied) => {
      setAssignError("");
      setEmptiedPlanNotice(emptied);
      void queryClient.invalidateQueries({ queryKey: ["admin", "scheduled-task-plans"] });
      onRefresh();
    },
    onError: (error: Error) => {
      setAssignError(error.message || "切换媒体库所属计划失败");
    },
  });

  const deleteEmptiedPlan = useMutation({
    mutationFn: (planId: string) => api.deleteAdminScheduledTaskPlan(planId),
    onSuccess: () => {
      setEmptiedPlanNotice(null);
      void queryClient.invalidateQueries({ queryKey: ["admin", "scheduled-task-plans"] });
      onRefresh();
    },
    onError: (error: Error) => {
      setAssignError(error.message || "删除空计划失败");
    },
  });

  const canCreateForActive = Boolean(
    activeGroup
    && activeGroup.scopeType === "LIBRARY"
    && creatableTaskTypeOptions.some(([type]) => type === activeGroup.taskType),
  );

  const openTopCreate = () => {
    if (creatableTaskTypeOptions.length === 0) return;
    const defaultType = canCreateForActive && activeGroup
      ? activeGroup.taskType
      : creatableTaskTypeOptions[0][0];
    setDialogState({
      mode: "create",
      taskType: defaultType,
      initialLibraryIds: [],
      lockTaskType: false,
    });
  };

  return (
    <section className="lux-admin-panel lux-operations-section" aria-labelledby="registered-plans-title">
      <div className="lux-operations-section-heading lux-registered-plans-heading">
        <div>
          <span className="lux-eyebrow">任务注册</span>
          <h2 id="registered-plans-title">已注册任务</h2>
          <p>左侧按任务类型查看覆盖范围；右侧独立配置每个时间档位与媒体库归属，触发后按媒体库排队执行。</p>
        </div>
        <div className="lux-registered-plans-tools">
          <label className="lux-registered-plans-search">
            <span className="lux-sr-only">搜索执行计划或媒体库</span>
            <LuxTextField
              aria-label="搜索执行计划或媒体库"
              type="search"
              value={search}
              onChange={(event) => onSearchChange(event.target.value)}
              placeholder="搜索计划或媒体库"
            />
          </label>
          <LuxButton
            tone="primary"
            size="compact"
            type="button"
            aria-label="新建执行计划"
            disabled={creatableTaskTypeOptions.length === 0}
            title={creatableTaskTypeOptions.length === 0 ? "暂无已注册媒体库任务可创建自定义计划" : undefined}
            onClick={openTopCreate}
          >
            <Plus size={15} />
            新建执行计划
          </LuxButton>
        </div>
      </div>

      {taskGroups.length === 0 ? (
        <div className="lux-operations-empty" role="status">
          <span className="lux-operations-empty-icon"><ClipboardList size={22} /></span>
          <div>
            <strong>{search.trim() ? `没有匹配“${search.trim()}”的执行计划` : "还没有执行计划"}</strong>
            <p>{search.trim() ? "可以修改关键词，或清除筛选查看全部任务。" : "系统功能或插件注册后台工作后，执行计划会显示在这里。"}</p>
          </div>
          {search.trim() ? (
            <LuxButton tone="secondary" size="compact" type="button" onClick={() => onSearchChange("")}>
              清除筛选
            </LuxButton>
          ) : null}
        </div>
      ) : (
        <div className="lux-workbench">
          <aside className="lux-workbench-nav" aria-label="已注册任务列表">
            {libraryGroups.length > 0 ? (
              <div className="lux-workbench-nav-section">
                <div className="lux-workbench-nav-label">媒体库任务（{libraryGroups.length}）</div>
                {libraryGroups.map((group) => (
                  <WorkbenchNavItem
                    key={group.taskType}
                    group={group}
                    totalLibraryCount={libraries.length}
                    selected={activeGroup?.taskType === group.taskType}
                    onSelect={() => {
                      setSelectedTaskType(group.taskType);
                      setAssignError("");
                    }}
                  />
                ))}
              </div>
            ) : null}
            {globalGroups.length > 0 ? (
              <div className="lux-workbench-nav-section">
                <div className="lux-workbench-nav-label">全局插件任务（{globalGroups.length}）</div>
                {globalGroups.map((group) => (
                  <WorkbenchNavItem
                    key={group.taskType}
                    group={group}
                    totalLibraryCount={libraries.length}
                    selected={activeGroup?.taskType === group.taskType}
                    onSelect={() => {
                      setSelectedTaskType(group.taskType);
                      setAssignError("");
                    }}
                  />
                ))}
              </div>
            ) : null}
          </aside>

          {activeGroup ? (
            <div className="lux-workbench-detail">
              <header className="lux-workbench-detail-head">
                <div>
                  <div className="lux-workbench-detail-title">
                    <h3>{activeGroup.label}</h3>
                    <code>{activeGroup.taskType}</code>
                    <span className="lux-workbench-scope-badge">
                      {activeGroup.scopeType === "GLOBAL"
                        ? "全局插件作用域"
                        : `支持 ${activeGroup.supportedLibraries.length} 个媒体库`}
                    </span>
                    <span className="lux-workbench-scope-badge">
                      {activeGroup.sourceType === "PLUGIN"
                        ? `插件注册${activeGroup.pluginId ? ` · ${activeGroup.pluginId}` : ""}`
                        : "系统注册"}
                    </span>
                  </div>
                  <p>{activeGroup.description}</p>
                </div>
                {canCreateForActive ? (
                  <LuxButton
                    tone="secondary"
                    size="compact"
                    type="button"
                    onClick={() => setDialogState({
                      mode: "create",
                      taskType: activeGroup.taskType,
                      initialLibraryIds: [],
                      lockTaskType: true,
                    })}
                  >
                    <Plus size={14} />
                    新建时间档位
                  </LuxButton>
                ) : null}
              </header>

              {runFeedback ? (
                <div className={`lux-workbench-run-feedback${runFeedback.skippedLibraryNames.length > 0 ? " is-warn" : ""}`} role="status">
                  <div className="lux-workbench-run-feedback-copy">
                    <CheckCircle2 size={16} aria-hidden="true" />
                    <div>
                      <strong>
                        {runFeedback.isGlobal
                          ? `已将「${runFeedback.planName}」加入执行队列`
                          : runFeedback.queuedCount > 0
                            ? `已将「${runFeedback.planName}」加入执行队列（${runFeedback.queuedCount} 个媒体库）`
                            : `「${runFeedback.planName}」未能新启动任务`}
                      </strong>
                      {runFeedback.skippedLibraryNames.length > 0 ? (
                        <small>已有同类任务运行中，已跳过：{runFeedback.skippedLibraryNames.join("、")}</small>
                      ) : null}
                    </div>
                  </div>
                  <div className="lux-workbench-run-feedback-actions">
                    <LuxButton tone="secondary" size="compact" type="button" aria-label="前往运行记录" onClick={onOpenRunsTab}>
                      查看运行记录
                      <ArrowRight size={14} />
                    </LuxButton>
                    <LuxIconButton size="small" type="button" aria-label="关闭执行提示" onClick={() => setRunFeedback(null)}>
                      <X size={14} />
                    </LuxIconButton>
                  </div>
                </div>
              ) : null}

              {emptiedPlanNotice ? (
                <div className="lux-workbench-empty-plan-notice" role="status">
                  <span>「{emptiedPlanNotice.name}」已没有关联媒体库，不会执行任何任务，可保留备用或直接删除。</span>
                  <div className="lux-workbench-run-feedback-actions">
                    <LuxButton
                      tone="danger"
                      size="compact"
                      type="button"
                      aria-label={`删除空计划${emptiedPlanNotice.name}`}
                      disabled={deleteEmptiedPlan.isPending}
                      onClick={() => deleteEmptiedPlan.mutate(emptiedPlanNotice.id)}
                    >
                      <Trash2 size={13} />
                      {deleteEmptiedPlan.isPending ? "删除中…" : "删除空计划"}
                    </LuxButton>
                    <LuxIconButton size="small" type="button" aria-label="关闭空计划提醒" onClick={() => setEmptiedPlanNotice(null)}>
                      <X size={14} />
                    </LuxIconButton>
                  </div>
                </div>
              ) : null}

              <section className="lux-workbench-block" aria-label="执行计划列表">
                <div className="lux-workbench-subhead">
                  <h4>{activeGroup.scopeType === "GLOBAL" ? "全局执行配置" : "时间档位 / 执行计划"}</h4>
                  <span>{activeGroup.visiblePlans.length} 个执行计划</span>
                </div>
                <div className="lux-registered-task-list">
                  {activeGroup.visiblePlans.map((plan) => (
                    <ScheduledTaskPlanRow
                      key={plan.id}
                      plan={plan}
                      libraries={libraries}
                      onEdit={() => setDialogState({ mode: "edit", plan })}
                      onSaved={onRefresh}
                      onDeleted={() => {
                        const remainingTotal = Math.max(0, total - 1);
                        const lastAvailablePage = Math.max(1, Math.ceil(remainingTotal / pageSize));
                        onPageChange(Math.min(page, lastAvailablePage));
                        onRefresh();
                      }}
                      onRunSuccess={(feedback) => setRunFeedback(feedback)}
                    />
                  ))}
                </div>
              </section>

              {activeGroup.scopeType === "LIBRARY" && activeGroup.supportedLibraries.length > 0 ? (
                <section className="lux-workbench-block" aria-label="按媒体库分配时间">
                  <div className="lux-workbench-subhead">
                    <h4>按媒体库快速调整时间归属（{activeGroup.supportedLibraries.length}）</h4>
                    <span>可直接把某个库切换到其他时间档位，或单独新建时间</span>
                  </div>
                  {assignError ? <p className="lux-error-copy" role="alert">{assignError}</p> : null}
                  <div className="lux-workbench-alloc-grid">
                    {activeGroup.supportedLibraries.map((library) => {
                      const currentPlan = activeGroup.allTaskPlans.find((plan) =>
                        (plan.libraries ?? []).some((member) => member.id === library.id),
                      );
                      const cronInfo = describeCronSchedule(currentPlan?.schedule);
                      const options = [
                        ...activeGroup.allTaskPlans.map((plan) => ({
                          value: plan.id,
                          label: `${plan.name || taskGroupLabel(plan.taskType, taskLabel)} (${describeCronSchedule(plan.schedule).localShort})`,
                        })),
                        ...(canCreateForActive ? [{
                          value: "__NEW__",
                          label: `+ 为「${library.name}」单独新建时间…`,
                        }] : []),
                      ];
                      return (
                        <div className="lux-workbench-alloc-card" key={library.id}>
                          <div className="lux-workbench-alloc-top">
                            <strong>{library.name}</strong>
                            <span className={`lux-registered-task-status ${currentPlan?.isEnabled && currentPlan?.schedule ? "is-enabled" : currentPlan?.schedule ? "is-disabled" : "is-unconfigured"}`}>
                              {cronInfo.localShort}
                            </span>
                          </div>
                          <LuxSelect
                            className="lux-workbench-alloc-select"
                            aria-label={`${library.name} 所属计划`}
                            value={currentPlan?.id ?? ""}
                            options={options}
                            disabled={quickAssign.isPending}
                            onChange={(targetPlanId) => {
                              if (targetPlanId === "__NEW__") {
                                setDialogState({
                                  mode: "create",
                                  taskType: activeGroup.taskType,
                                  initialLibraryIds: [library.id],
                                  lockTaskType: true,
                                });
                                return;
                              }
                              if (targetPlanId && targetPlanId !== currentPlan?.id) {
                                quickAssign.mutate({ targetPlanId, libraryId: library.id, taskType: activeGroup.taskType });
                              }
                            }}
                          />
                        </div>
                      );
                    })}
                  </div>
                </section>
              ) : null}

              {unsupportedLibraries.length > 0 ? (
                <details className="lux-workbench-unsupported">
                  <summary>另有 {unsupportedLibraries.length} 个媒体库未接入「{activeGroup.label}」</summary>
                  <p>
                    这些媒体库尚未注册该任务类型（可在媒体库或对应插件设置中开启）：
                    {unsupportedLibraries.map((lib) => lib.name).join("、")}。
                  </p>
                </details>
              ) : null}
            </div>
          ) : null}
        </div>
      )}

      {plans.length > 0 && total > pageSize ? pagination : null}

      {dialogState ? (
        <PlanEditorDialog
          dialogState={dialogState}
          allPlans={baselinePlans}
          creatableTaskTypeOptions={creatableTaskTypeOptions}
          libraries={libraries}
          taskLabel={taskLabel}
          onClose={() => setDialogState(null)}
          onSaved={() => {
            setDialogState(null);
            onRefresh();
          }}
        />
      ) : null}
    </section>
  );
}

function WorkbenchNavItem({
  group,
  totalLibraryCount,
  selected,
  onSelect,
}: {
  group: TaskGroupSummary;
  totalLibraryCount: number;
  selected: boolean;
  onSelect: () => void;
}) {
  const isGlobal = group.scopeType === "GLOBAL";
  const enabledCount = group.visiblePlans.filter((plan) => plan.isEnabled && Boolean(plan.schedule)).length;
  const globalPlan = group.visiblePlans[0];
  const scopeText = isGlobal
    ? "全局"
    : totalLibraryCount > 0
      ? `${group.supportedLibraries.length}/${totalLibraryCount} 库`
      : `${group.supportedLibraries.length} 个媒体库`;
  const subText = isGlobal
    ? describeCronSchedule(globalPlan?.schedule).localShort
    : `${group.visiblePlans.length} 个执行计划 · ${enabledCount > 0 ? `${enabledCount} 个定时启用` : "仅手动"}`;

  return (
    <button
      type="button"
      className={`lux-workbench-task-item${selected ? " is-selected" : ""}`}
      data-workbench-task={group.taskType}
      aria-current={selected ? "true" : undefined}
      onClick={onSelect}
    >
      <span className="lux-workbench-task-top">
        <strong>{group.label}</strong>
        <span className="lux-workbench-task-scope">{scopeText}</span>
      </span>
      <span className="lux-workbench-task-sub">{subText}</span>
    </button>
  );
}

function ScheduledTaskPlanRow({
  plan,
  libraries,
  onEdit,
  onSaved,
  onDeleted,
  onRunSuccess,
}: {
  plan: AdminScheduledTaskPlan;
  libraries: AdminLibrary[];
  onEdit: () => void;
  onSaved: () => void;
  onDeleted: () => void;
  onRunSuccess: (feedback: RunFeedback) => void;
}) {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const globalPlan = plan.scopeType === "GLOBAL";
  const name = plan.name || plan.taskName || plan.taskType;
  const configured = Boolean(plan.schedule);
  const knownLibraries = (plan.libraries ?? []).length || plan.libraryCount || 0;
  const hasLibraries = globalPlan || knownLibraries > 0;
  const stateLabel = !hasLibraries
    ? "未分配媒体库"
    : plan.isEnabled && configured
      ? "已启用"
      : configured
        ? "已停用"
        : "未配置计划";
  const canDelete = !globalPlan && plan.isDefault !== true;
  const cronInfo = describeCronSchedule(plan.schedule);

  const toggleEnabled = useMutation({
    mutationFn: (nextEnabled: boolean) => api.updateAdminScheduledTaskPlan(plan.id, { isEnabled: nextEnabled }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "scheduled-task-plans"] });
      onSaved();
    },
  });

  const runNow = useMutation({
    mutationFn: async () => globalPlan
      ? {
          kind: "global" as const,
          result: await api.runAdminScheduledTask({ ownerType: "GLOBAL", ownerId: "global", taskType: plan.taskType }),
        }
      : {
          kind: "plan" as const,
          result: await api.runAdminScheduledTaskPlan(plan.id),
        },
    onSuccess: (payload) => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminJobs() });
      void queryClient.invalidateQueries({ queryKey: ["admin", "metadata-jobs"] });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminStrmProbeJobs() });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminChapterDetectionJobs() });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminDanmakuMatchJobs() });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraryCoverJobs() });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminTaskActivity });
      if (payload.kind === "plan") {
        const libNameById = new Map([
          ...libraries.map((lib) => [lib.id, lib.name] as const),
          ...(plan.libraries ?? []).map((lib) => [lib.id, lib.name] as const),
        ]);
        onRunSuccess({
          planName: name,
          queuedCount: payload.result.runs?.length ?? knownLibraries,
          skippedLibraryNames: (payload.result.skippedLibraryIds ?? []).map((id) => libNameById.get(id) ?? id),
          isGlobal: false,
        });
      } else {
        onRunSuccess({
          planName: name,
          queuedCount: 1,
          skippedLibraryNames: [],
          isGlobal: true,
        });
      }
      onSaved();
    },
  });

  const deletePlan = useMutation({
    mutationFn: () => api.deleteAdminScheduledTaskPlan(plan.id),
    onSuccess: onDeleted,
  });

  const handleDelete = async () => {
    const prompt = knownLibraries === 0
      ? `“${name}”当前未关联任何媒体库，确定删除此空计划吗？`
      : `删除“${name}”后，${knownLibraries} 个媒体库将回到默认计划，并恢复默认 Cron 和启停状态。确定删除吗？`;
    if (!(await confirm(prompt))) {
      return;
    }
    deletePlan.mutate();
  };

  return (
    <article className="lux-registered-task-row lux-registered-plan-row">
      <div className="lux-registered-task-icon"><ClipboardList size={18} /></div>
      <div className="lux-registered-task-copy">
        <div className="lux-registered-task-heading">
          <strong>{name}</strong>
          <span className={`lux-registered-task-status ${hasLibraries && plan.isEnabled && configured ? "is-enabled" : hasLibraries && configured ? "is-disabled" : "is-unconfigured"}`}>
            {stateLabel}
          </span>
        </div>
        <div className="lux-registered-task-meta">
          <span>{globalPlan ? "全局插件计划" : `${knownLibraries} 个媒体库`}</span>
          <span>{plan.isDefault ? "默认计划" : "自定义计划"}</span>
          {plan.sourceType === "PLUGIN"
            ? <span>插件注册{plan.pluginId ? ` · ${plan.pluginId}` : ""}</span>
            : <span>系统注册</span>}
        </div>
        {globalPlan ? null : (
          <div className="lux-registered-plan-libraries">
            {(plan.libraries ?? []).map((library) => <span key={library.id}>{library.name}</span>)}
          </div>
        )}
        <div className="lux-workbench-cron-readout">
          <strong>{cronInfo.human}</strong>
          <small>{cronInfo.sub}</small>
        </div>
      </div>

      <div className="lux-registered-task-actions">
        {globalPlan ? null : (
          <LuxSwitch
            className="lux-admin-toggle lux-workbench-row-switch"
            aria-label={`启停${name}`}
            checked={plan.isEnabled}
            disabled={!hasLibraries || !configured || toggleEnabled.isPending || deletePlan.isPending}
            title={!hasLibraries ? "未分配媒体库，请先分配媒体库" : configured ? "快速启用或停用此计划" : "请先配置 Cron 执行计划"}
            onChange={(event) => toggleEnabled.mutate(event.target.checked)}
          />
        )}
        <LuxButton
          tone="secondary"
          size="compact"
          className="lux-registered-task-run"
          type="button"
          aria-label={`立即执行${name}`}
          onClick={() => runNow.mutate()}
          disabled={!hasLibraries || runNow.isPending || deletePlan.isPending}
          title={!hasLibraries ? "未分配媒体库，无法执行" : undefined}
        >
          <Play size={14} />
          {runNow.isPending ? "执行中…" : "立即执行"}
        </LuxButton>
        <LuxIconButton
          className="lux-icon-button-small lux-registered-task-edit"
          type="button"
          aria-label={`编辑${name}`}
          onClick={onEdit}
          disabled={deletePlan.isPending}
        >
          <Pencil size={15} />
        </LuxIconButton>
        {canDelete ? (
          <LuxIconButton
            danger
            className="lux-icon-button-small"
            type="button"
            aria-label={`删除${name}`}
            onClick={handleDelete}
            disabled={deletePlan.isPending}
            title="删除执行计划"
          >
            <Trash2 size={15} />
          </LuxIconButton>
        ) : null}
      </div>

      {toggleEnabled.error ? <p className="lux-error-copy lux-registered-task-error" role="alert">{toggleEnabled.error.message}</p> : null}
      {runNow.error ? <p className="lux-error-copy lux-registered-task-error" role="alert">{runNow.error.message}</p> : null}
      {deletePlan.error ? <p className="lux-error-copy lux-registered-task-error" role="alert">{deletePlan.error.message}</p> : null}
    </article>
  );
}

function PlanEditorDialog({
  dialogState,
  allPlans,
  creatableTaskTypeOptions,
  libraries,
  taskLabel,
  onClose,
  onSaved,
}: {
  dialogState: DialogState;
  allPlans: AdminScheduledTaskPlan[];
  creatableTaskTypeOptions: Array<[string, string]>;
  libraries: AdminLibrary[];
  taskLabel: (taskType: string) => string;
  onClose: () => void;
  onSaved: () => void;
}) {
  const isEdit = dialogState.mode === "edit";
  const editingPlan = dialogState.mode === "edit" ? dialogState.plan : null;
  const isGlobal = editingPlan?.scopeType === "GLOBAL";
  const isDefault = editingPlan?.isDefault === true;

  const [taskType, setTaskType] = useState<string>(
    dialogState.mode === "edit" ? dialogState.plan.taskType : dialogState.taskType,
  );
  const [name, setName] = useState<string>(editingPlan ? editingPlan.name : "");
  const [schedule, setSchedule] = useState<string>(
    editingPlan ? (editingPlan.schedule ?? "") : buildCronPresets(true)[0].cron,
  );
  const [enabled, setEnabled] = useState<boolean>(
    editingPlan ? editingPlan.isEnabled : true,
  );
  const [selectedIds, setSelectedIds] = useState<string[]>(
    dialogState.mode === "edit"
      ? (dialogState.plan.libraries ?? []).map((lib) => lib.id)
      : dialogState.initialLibraryIds,
  );
  const [validationError, setValidationError] = useState<string>("");

  const taskPlans = useMemo(
    () => allPlans.filter((plan) => plan.taskType === taskType),
    [allPlans, taskType],
  );

  // Libraries available for this taskType: registered libraries from plans, falling back to full library list when no plans are cached yet.
  const selectableLibraries = useMemo(() => {
    const libraryNameMap = new Map(libraries.map((lib) => [lib.id, lib.name]));
    const registeredMap = new Map<string, string>();
    for (const plan of taskPlans) {
      for (const lib of plan.libraries ?? []) {
        registeredMap.set(lib.id, libraryNameMap.get(lib.id) ?? lib.name);
      }
    }
    if (registeredMap.size > 0) {
      return [...registeredMap.entries()].map(([id, libName]) => ({ id, name: libName }));
    }
    return libraries.map((lib) => ({ id: lib.id, name: lib.name }));
  }, [libraries, taskPlans]);

  const ownerByLibraryId = useMemo(() => {
    const map = new Map<string, AdminScheduledTaskPlan>();
    for (const plan of taskPlans) {
      for (const lib of plan.libraries ?? []) {
        map.set(lib.id, plan);
      }
    }
    return map;
  }, [taskPlans]);

  const lockedDefaultLibraryIds = useMemo(() => {
    if (!isDefault || !editingPlan) return new Set<string>();
    return new Set((editingPlan.libraries ?? []).map((lib) => lib.id));
  }, [editingPlan, isDefault]);

  const cronPresets = useMemo(() => buildCronPresets(!isGlobal), [isGlobal]);
  const cronInfo = describeCronSchedule(schedule);

  const mutation = useMutation({
    mutationFn: async () => {
      const normalizedSchedule = schedule.trim() || null;
      if (isGlobal && editingPlan) {
        await api.updateAdminScheduledTask({
          ownerType: "GLOBAL",
          ownerId: "global",
          taskType: editingPlan.taskType,
          schedule: normalizedSchedule,
          isEnabled: undefined,
        });
        return;
      }
      if (isEdit && editingPlan) {
        await api.updateAdminScheduledTaskPlan(editingPlan.id, {
          name: name.trim(),
          schedule: normalizedSchedule,
          isEnabled: normalizedSchedule ? enabled : false,
          libraryIds: selectedIds,
        });
        return;
      }
      await api.createAdminScheduledTaskPlan({
        taskType,
        name: name.trim(),
        schedule: normalizedSchedule,
        isEnabled: normalizedSchedule ? enabled : false,
        libraryIds: selectedIds,
      });
    },
    onSuccess: onSaved,
  });

  const handleSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setValidationError("");
    if (isGlobal && !schedule.trim()) {
      setValidationError("全局插件任务必须保留 Cron 执行计划。");
      return;
    }
    if (!cronInfo.valid) {
      setValidationError(cronInfo.human);
      return;
    }
    if (!isGlobal) {
      if (!name.trim()) {
        setValidationError("请填写计划名称。");
        return;
      }
      if (selectedIds.length === 0) {
        setValidationError("请至少勾选 1 个媒体库。");
        return;
      }
    }
    mutation.mutate();
  };

  const toggleLibrary = (libraryId: string) => {
    if (lockedDefaultLibraryIds.has(libraryId)) return;
    setValidationError("");
    setSelectedIds((prev) =>
      prev.includes(libraryId)
        ? prev.filter((id) => id !== libraryId)
        : [...prev, libraryId],
    );
  };

  const selectAllLibraries = () => {
    setValidationError("");
    setSelectedIds(selectableLibraries.map((lib) => lib.id));
  };

  const currentTaskLabel = taskGroupLabel(taskType, taskLabel);
  const title = isEdit ? `编辑「${editingPlan?.name || currentTaskLabel}」` : `新建「${currentTaskLabel}」执行计划`;
  const description = isGlobal
    ? "全局插件任务：只可修改 Cron 定时（必填），启停与媒体库范围由插件自身配置管理。"
    : isDefault
      ? "默认计划用于兜底未拆分的媒体库；无法直接移除已有媒体库，如需拆分时间请新建自定义计划。"
      : "支持同时修改计划名称、定时与多选媒体库；从其他计划勾选的媒体库将在保存后自动移入。";

  return (
    <LuxDialog title={title} description={description} size="wide" onClose={onClose}>
      <form className="lux-plan-dialog-form" onSubmit={handleSubmit}>
        {dialogState.mode === "create" && !dialogState.lockTaskType ? (
          <label className="lux-plan-dialog-field">
            <span>任务类型</span>
            <LuxSelect
              value={taskType}
              options={(creatableTaskTypeOptions.length > 0 ? creatableTaskTypeOptions : SCHEDULE_TASK_TYPE_OPTIONS).map(([value, label]) => ({ value, label }))}
              onChange={(nextType) => {
                setTaskType(nextType);
                setSelectedIds([]);
                setValidationError("");
              }}
              aria-label="执行计划任务类型"
            />
          </label>
        ) : null}

        {isGlobal ? null : (
          <label className="lux-plan-dialog-field">
            <span>计划名称</span>
            <LuxTextField
              aria-label="计划名称"
              value={name}
              onChange={(event) => {
                setName(event.target.value);
                setValidationError("");
              }}
              maxLength={128}
              placeholder="例如：夜间校验"
              required
            />
          </label>
        )}

        <div className="lux-plan-dialog-field">
          <span>常用时间预设（自动换算为 UTC 执行）</span>
          <div className="lux-plan-preset-row">
            {cronPresets.map((preset) => (
              <button
                key={preset.label}
                type="button"
                className={`lux-plan-preset-chip${schedule.trim() === preset.cron ? " is-active" : ""}`}
                onClick={() => {
                  setSchedule(preset.cron);
                  setValidationError("");
                  if (preset.cron && !isEdit) setEnabled(true);
                }}
              >
                {preset.label}
              </button>
            ))}
          </div>
        </div>

        <label className="lux-plan-dialog-field">
          <span>Cron 执行计划（五段式：分 时 日 月 周，按 UTC 执行{isGlobal ? "，必填" : "，留空表示仅手动"}）</span>
          <LuxTextField
            aria-label="Cron 执行计划"
            value={schedule}
            onChange={(event) => {
              setSchedule(event.target.value);
              setValidationError("");
            }}
            maxLength={128}
            placeholder={isGlobal ? "例如 0 3 * * *" : "留空表示仅手动，例如 0 3 * * *"}
            required={isGlobal}
          />
        </label>

        <div className={`lux-plan-cron-preview${cronInfo.valid ? "" : " is-invalid"}`} role="status">
          <strong>{cronInfo.human}</strong>
          <small>{cronInfo.sub}</small>
        </div>

        {isGlobal ? null : (
          <>
            <LuxSwitch
              className="lux-admin-toggle"
              checked={Boolean(schedule.trim()) && enabled}
              disabled={!schedule.trim()}
              onChange={(event) => setEnabled(event.target.checked)}
              label={schedule.trim() ? "启用此定时计划" : "未配置 Cron 时仅支持手动执行"}
            />

            <div className="lux-plan-dialog-field">
              <div className="lux-plan-checklist-head">
                <span>适用媒体库（已选 {selectedIds.length} / {selectableLibraries.length}）</span>
                {!isDefault && selectableLibraries.length > 0 ? (
                  <LuxButton tone="secondary" size="compact" type="button" onClick={selectAllLibraries}>
                    全选 ({selectableLibraries.length})
                  </LuxButton>
                ) : null}
              </div>
              <div className="lux-plan-checklist" role="group" aria-label="选择目标媒体库">
                {selectableLibraries.length === 0 ? (
                  <p className="lux-admin-muted">暂无可选媒体库。</p>
                ) : selectableLibraries.map((library) => {
                  const checked = selectedIds.includes(library.id);
                  const locked = lockedDefaultLibraryIds.has(library.id);
                  const ownerPlan = ownerByLibraryId.get(library.id);
                  const originText = locked
                    ? "默认兜底库（不可直接移除）"
                    : ownerPlan
                      ? ownerPlan.id === editingPlan?.id
                        ? "当前在本计划"
                        : `当前在「${ownerPlan.name}」${checked ? " · 保存后移入" : ""}`
                      : "未归属";
                  return (
                    <div className={`lux-plan-check-row${locked ? " is-locked" : ""}`} key={library.id}>
                      <LuxCheckbox
                        checked={checked}
                        disabled={locked}
                        data-library-id={library.id}
                        aria-label={`选择媒体库 ${library.name}`}
                        label={library.name}
                        onChange={() => toggleLibrary(library.id)}
                      />
                      <span className={`lux-plan-check-origin${ownerPlan && ownerPlan.id !== editingPlan?.id && checked ? " is-move" : ""}`}>
                        {originText}
                      </span>
                    </div>
                  );
                })}
              </div>
            </div>
          </>
        )}

        {validationError ? <p className="lux-error-copy" role="alert">{validationError}</p> : null}
        {mutation.error ? <p className="lux-error-copy" role="alert">{mutation.error.message}</p> : null}

        <div className="lux-plan-dialog-actions">
          <LuxButton tone="secondary" type="button" onClick={onClose}>取消</LuxButton>
          <LuxButton tone="primary" type="submit" disabled={mutation.isPending}>
            <Save size={15} />
            {mutation.isPending ? "保存中…" : isEdit ? "保存计划" : "创建执行计划"}
          </LuxButton>
        </div>
      </form>
    </LuxDialog>
  );
}

function taskGroupLabel(taskType: string, fallback: (taskType: string) => string) {
  return SCHEDULE_TASK_TYPE_OPTIONS.find(([value]) => value === taskType)?.[1] ?? fallback(taskType);
}
