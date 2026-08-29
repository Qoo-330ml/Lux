import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminEmbyMigrationImport, AdminEmbyMigrationMatch, AdminEmbyMigrationPage, AdminEmbyMigrationPersonFavorite, AdminEmbyMigrationUserLink } from "../../lib/api/types";

export type ReportTab = "users" | "matches" | "imports" | "personFavorites";
type ReportEntry = AdminEmbyMigrationUserLink | AdminEmbyMigrationMatch | AdminEmbyMigrationImport | AdminEmbyMigrationPersonFavorite;
export type MigrationReportData = AdminEmbyMigrationPage<ReportEntry>;

const pageSize = 50;

export function useMigrationReport(jobId: string | null, tab: ReportTab, page: number, enabled = true) {
  return useQuery<MigrationReportData>({
    queryKey: jobId ? reportKey(tab, jobId, page) : ["admin", "emby-migration-report", "none", tab, page],
    queryFn: async (): Promise<MigrationReportData> => {
      if (!jobId) return { page, pageSize, users: [], matches: [], imports: [], personFavorites: [] };
      if (tab === "users") return api.adminEmbyMigrationUsers(jobId, page);
      if (tab === "matches") return api.adminEmbyMigrationMatches(jobId, page);
      if (tab === "personFavorites") return api.adminEmbyMigrationPersonFavorites(jobId, page);
      return api.adminEmbyMigrationImports(jobId, page);
    },
    enabled: Boolean(jobId) && enabled,
    retry: false,
  });
}

export function EmbyMigrationReports({ jobId, tab, report, onTabChange, onPageChange }: { jobId: string; tab: ReportTab; report: ReturnType<typeof useMigrationReport>; onTabChange: (tab: ReportTab) => void; onPageChange: (page: number) => void }) {
  return (
    <div className="lux-emby-report-content">
      <div className="lux-emby-report-tabs" role="tablist" aria-label="迁移报告">
        <ReportTabButton active={tab === "users"} onClick={() => onTabChange("users")}>用户关联</ReportTabButton>
        <ReportTabButton active={tab === "matches"} onClick={() => onTabChange("matches")}>媒体匹配</ReportTabButton>
        <ReportTabButton active={tab === "imports"} onClick={() => onTabChange("imports")}>导入结果</ReportTabButton>
        <ReportTabButton active={tab === "personFavorites"} onClick={() => onTabChange("personFavorites")}>人物收藏</ReportTabButton>
      </div>
      {report.isPending ? <LoadingState label="正在读取报告…" /> : report.error ? <InlineError message={report.error.message} /> : <ReportContent tab={tab} data={report.data} page={report.data?.page ?? 1} onPageChange={onPageChange} />}
    </div>
  );
}

function ReportContent({ tab, data, page, onPageChange }: { tab: ReportTab; data?: MigrationReportData; page: number; onPageChange: (page: number) => void }) {
  const entries = data ? reportEntries(data, tab) : [];
  const currentPage = data?.page ?? page;
  const responsePageSize = data?.pageSize ?? pageSize;
  if (!entries.length) return <EmptyState label="此报告暂无记录" detail="任务运行后，这里会显示可核对的迁移结果。" />;
  return <>
    <div className="lux-emby-report-table">{entries.map((entry, index) => <ReportRow key={`${entry.jobId}-${index}`} tab={tab} entry={entry} />)}</div>
    <div className="lux-emby-pagination"><button className="lux-button lux-button-secondary" type="button" disabled={currentPage <= 1} onClick={() => onPageChange(currentPage - 1)}>上一页</button><span>第 {currentPage} 页</span><button className="lux-button lux-button-secondary" type="button" disabled={entries.length < responsePageSize} onClick={() => onPageChange(currentPage + 1)}>下一页</button></div>
  </>;
}

function ReportRow({ tab, entry }: { tab: ReportTab; entry: ReportEntry }) {
  if (tab === "users") {
    const user = entry as AdminEmbyMigrationUserLink;
    return <div className="lux-emby-report-row"><div><strong>{user.embyUsername}</strong><small>Emby ID：{user.embyUserId}</small></div><ReportStatus status={user.status} /><span>{user.luxUserId ? `Lux 用户：${user.luxUserId}` : user.error ?? "未关联"}</span></div>;
  }
  if (tab === "matches") {
    const match = entry as AdminEmbyMigrationMatch;
    return <div className="lux-emby-report-row"><div><strong>{match.detail.title ? String(match.detail.title) : match.embyItemId}</strong><small>{match.embyItemType} · {match.matchMethod}{match.confidence == null ? "" : ` · ${match.confidence}%`}</small></div><ReportStatus status={match.status} /><span>{match.luxItemId ? `Lux 媒体：${luxMediaLabel(match.detail, match.luxItemId)}` : safeDetail(match.detail)}</span></div>;
  }
  if (tab === "imports") {
    const imported = entry as AdminEmbyMigrationImport;
    return <div className="lux-emby-report-row"><div><strong>{imported.embyItemId}</strong><small>Emby 用户：{imported.embyUserId}</small></div><ReportStatus status={imported.status} /><span>{imported.error ?? "状态已写入 Lux"}</span></div>;
  }
  const favorite = entry as AdminEmbyMigrationPersonFavorite;
  return <div className="lux-emby-report-row"><div><strong>{favorite.embyPersonName}</strong><small>Emby 用户：{favorite.embyUserId} · Person ID：{favorite.embyPersonId}</small></div><ReportStatus status={favorite.status} /><span>{favorite.luxPersonId ? `Lux 人物：${favorite.luxPersonId}` : `${favorite.matchMethod}${favorite.confidence == null ? "" : ` · ${favorite.confidence}%`} · ${favorite.error ?? "未匹配"}`}</span></div>;
}

function reportEntries(data: MigrationReportData, tab: ReportTab): ReportEntry[] {
  if (tab === "users") return data.users ?? [];
  if (tab === "matches") return data.matches ?? [];
  if (tab === "imports") return data.imports ?? [];
  return data.personFavorites ?? [];
}

function reportKey(tab: ReportTab, jobId: string, page: number) {
  if (tab === "users") return queryKeys.adminEmbyMigrationUsers(jobId, page);
  if (tab === "matches") return queryKeys.adminEmbyMigrationMatches(jobId, page);
  if (tab === "personFavorites") return queryKeys.adminEmbyMigrationPersonFavorites(jobId, page);
  return queryKeys.adminEmbyMigrationImports(jobId, page);
}

function ReportTabButton({ active, children, onClick }: { active: boolean; children: string; onClick: () => void }) { return <button className={active ? "is-active" : ""} type="button" role="tab" aria-selected={active} onClick={onClick}>{children}</button>; }
function ReportStatus({ status }: { status: string }) { return <span className={`lux-emby-report-status is-${status.toLowerCase()}`}>{statusLabel(status)}</span>; }
function InlineError({ message }: { message: string }) { return <p className="lux-error-copy lux-emby-inline-error" role="alert">{message}</p>; }
function LoadingState({ label }: { label: string }) { return <div className="lux-emby-state" role="status">{label}</div>; }
function EmptyState({ label, detail }: { label: string; detail: string }) { return <div className="lux-emby-empty"><strong>{label}</strong><span>{detail}</span></div>; }
function statusLabel(status: string) { return ({ PENDING: "等待中", RUNNING: "进行中", COMPLETED: "已完成", CANCELLED: "已取消", FAILED: "失败" } as Record<string, string>)[status] ?? status; }
function safeDetail(detail: Record<string, unknown>) { const copy = Object.fromEntries(Object.entries(detail).filter(([key]) => !key.toLowerCase().includes("url") && !key.toLowerCase().includes("token"))); return JSON.stringify(copy); }
function luxMediaLabel(detail: Record<string, unknown>, luxItemId: string) {
  const title = detailText(detail.luxTitle);
  const seriesTitle = detailText(detail.luxSeriesTitle);
  const season = detailNumber(detail.luxSeasonNumber);
  const episode = detailNumber(detail.luxEpisodeNumber);
  const parts = [seriesTitle || title];
  if (season != null) parts.push(`第 ${season} 季`);
  if (episode != null) parts.push(`第 ${episode} 集`);
  if (title && title !== seriesTitle && (season != null || episode != null)) parts.push(title);
  return parts.filter(Boolean).join(" · ") || luxItemId;
}
function detailText(value: unknown) { return typeof value === "string" && value.trim() ? value.trim() : null; }
function detailNumber(value: unknown) { return typeof value === "number" && Number.isFinite(value) ? value : null; }
