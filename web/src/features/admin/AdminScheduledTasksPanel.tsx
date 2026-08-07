import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarClock, Pencil, Power, Save } from "lucide-react";
import { useState, type FormEvent } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminScheduledTask } from "../../lib/api/types";
import { formatAdminDate } from "./date";

const SCHEDULE_TASKS = [
  { value: "INCREMENTAL_SCAN", label: "增量扫描" },
  { value: "RECONCILIATION_SCAN", label: "全量校验" },
  { value: "METADATA_PARSE", label: "元数据任务" },
] as const;

type ScheduledTaskInput = {
  ownerType: "GLOBAL" | "LIBRARY";
  ownerId: string;
  taskType: string;
  schedule: string | null;
  isEnabled?: boolean;
};

export function AdminScheduledTasksPanel() {
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const schedules = useQuery({ queryKey: queryKeys.adminScheduledTasks(page), queryFn: () => api.adminScheduledTasks(page) });
  const libraries = useQuery({ queryKey: queryKeys.adminLibraries, queryFn: () => api.adminLibraries() });
  const [ownerType, setOwnerType] = useState<"GLOBAL" | "LIBRARY">("GLOBAL");
  const [ownerId, setOwnerId] = useState("global");
  const [taskType, setTaskType] = useState<(typeof SCHEDULE_TASKS)[number]["value"]>("INCREMENTAL_SCAN");
  const [schedule, setSchedule] = useState("");
  const [isEnabled, setIsEnabled] = useState(true);
  const [savedMessage, setSavedMessage] = useState("");
  const update = useMutation({
    mutationFn: (input: ScheduledTaskInput) => api.updateAdminScheduledTask(input),
    onSuccess: () => {
      setSavedMessage("计划已保存");
      void queryClient.invalidateQueries({ queryKey: ["admin", "scheduled-tasks"] });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
    onError: () => setSavedMessage("计划保存失败，请检查格式后重试"),
  });

  const libraryItems = libraries.data?.libraries ?? [];
  const scheduleItems = schedules.data?.scheduledTasks ?? [];
  const total = schedules.data?.total ?? 0;
  const pageSize = schedules.data?.pageSize ?? 100;
  const totalPages = Math.max(1, Math.ceil(total / pageSize));
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setSavedMessage("");
    update.mutate({ ownerType, ownerId, taskType, schedule: schedule.trim() || null, isEnabled: isEnabled && Boolean(schedule.trim()) });
  };

  return (
    <section className="lux-admin-panel lux-admin-operations-section" aria-labelledby="scheduled-tasks-heading">
      <div className="lux-admin-panel-heading">
        <div><span className="lux-eyebrow">SCHEDULED TASKS</span><h2 id="scheduled-tasks-heading">计划任务</h2></div>
        <CalendarClock size={20} className="lux-admin-panel-icon" aria-hidden="true" />
      </div>
      <p className="lux-admin-muted">集中管理增量扫描、全量校验和元数据任务，可分别作用于全局或指定媒体库。</p>
      <form className="lux-admin-form lux-admin-schedule-form" onSubmit={submit}>
        <label htmlFor="schedule-owner">作用范围<select id="schedule-owner" name="schedule-owner" value={`${ownerType}:${ownerId}`} onChange={(event) => {
          const [nextOwnerType, nextOwnerId] = event.target.value.split(":", 2);
          if (nextOwnerType === "GLOBAL") {
            setOwnerType("GLOBAL");
            setOwnerId("global");
          } else if (nextOwnerType === "LIBRARY" && nextOwnerId) {
            setOwnerType("LIBRARY");
            setOwnerId(nextOwnerId);
          }
        }}>
          <option value="GLOBAL:global">全局</option>
          {libraryItems.filter((library) => library.isEnabled).map((library) => <option key={library.id} value={`LIBRARY:${library.id}`}>{library.name}</option>)}
        </select></label>
        <label htmlFor="schedule-task-type">任务类型<select id="schedule-task-type" name="schedule-task-type" value={taskType} onChange={(event) => setTaskType(event.target.value as (typeof SCHEDULE_TASKS)[number]["value"])}>
          {SCHEDULE_TASKS.map((task) => <option key={task.value} value={task.value}>{task.label}</option>)}
        </select></label>
        <label htmlFor="schedule-expression">执行计划<input id="schedule-expression" name="schedule-expression" value={schedule} onChange={(event) => setSchedule(event.target.value)} placeholder="interval:1h 或 0 3 * * *" /></label>
        <label className="lux-admin-toggle" htmlFor="schedule-enabled"><input id="schedule-enabled" type="checkbox" checked={isEnabled} onChange={(event) => setIsEnabled(event.target.checked)} /><span>启用</span></label>
        <button className="lux-button lux-button-secondary" type="submit" disabled={update.isPending}><Save size={15} />{update.isPending ? "保存中…" : "保存计划"}</button>
      </form>
      {savedMessage ? <p className="lux-admin-muted" role="status">{savedMessage}</p> : null}
      {schedules.isPending || libraries.isPending ? <p className="lux-admin-muted" role="status">正在读取计划…</p> : null}
      {schedules.error || libraries.error ? <p className="lux-error-copy" role="alert">计划任务暂时无法读取。</p> : null}
      {!schedules.isPending && !schedules.error ? <div className="lux-admin-schedule-list">
        {scheduleItems.length === 0 ? <p className="lux-admin-muted">还没有保存的计划任务。</p> : scheduleItems.map((task) => <ScheduleRow key={`${task.ownerType}-${task.ownerId}-${task.taskType}`} task={task} onEdit={() => { setOwnerType(task.ownerType === "GLOBAL" ? "GLOBAL" : "LIBRARY"); setOwnerId(task.ownerType === "GLOBAL" ? "global" : task.ownerId); setTaskType(task.taskType as (typeof SCHEDULE_TASKS)[number]["value"]); setSchedule(task.schedule ?? ""); setIsEnabled(task.isEnabled); }} onDisable={() => update.mutate({ ownerType: task.ownerType === "GLOBAL" ? "GLOBAL" : "LIBRARY", ownerId: task.ownerType === "GLOBAL" ? "global" : task.ownerId, taskType: task.taskType, schedule: null, isEnabled: false })} />)}
        {totalPages > 1 ? <div className="lux-admin-pagination"><span>第 {page} / {totalPages} 页，共 {total} 个计划</span><div><button className="lux-button lux-button-secondary" type="button" onClick={() => setPage((current) => Math.max(1, current - 1))} disabled={page === 1}>上一页</button><button className="lux-button lux-button-secondary" type="button" onClick={() => setPage((current) => Math.min(totalPages, current + 1))} disabled={page === totalPages}>下一页</button></div></div> : null}
      </div> : null}
    </section>
  );
}

function ScheduleRow({ task, onEdit, onDisable }: { task: AdminScheduledTask; onEdit: () => void; onDisable: () => void }) {
  const taskLabel = SCHEDULE_TASKS.find((item) => item.value === task.taskType)?.label ?? task.taskType;
  const canEdit = (task.ownerType === "GLOBAL" || task.ownerType === "LIBRARY") && SCHEDULE_TASKS.some((item) => item.value === task.taskType);
  return <article className="lux-admin-schedule-row"><div className="lux-job-icon"><CalendarClock size={16} /></div><div className="lux-admin-job-main"><div className="lux-admin-job-heading"><strong>{task.ownerName || "全局"} · {taskLabel}</strong><span className={`lux-job-status ${task.isEnabled ? "status-completed" : "status-cancelled"}`}>{task.isEnabled ? "已启用" : "未启用"}</span></div><small>{task.isEnabled && task.schedule ? task.schedule : "未设置执行计划"}{task.updatedAt ? ` · 更新于 ${formatAdminDate(task.updatedAt)}` : ""}</small></div><div className="lux-admin-job-actions">{canEdit ? <button className="lux-icon-button lux-icon-button-small" type="button" aria-label={`编辑${taskLabel}`} onClick={onEdit}><Pencil size={15} /></button> : null}{canEdit && task.isEnabled ? <button className="lux-icon-button lux-icon-button-small" type="button" aria-label={`停用${taskLabel}`} onClick={onDisable}><Power size={15} /></button> : null}</div></article>;
}
