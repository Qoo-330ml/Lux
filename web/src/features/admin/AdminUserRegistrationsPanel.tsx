import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { api, ApiError } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type {
  AdminLibrary,
  AdminUserLibraryAccessMode,
  AdminUserRegistration,
  NewUserPermissions,
} from "../../lib/api/types";
import { LuxButton, LuxCheckbox, LuxSelectField, LuxTextField, useLuxConfirm } from "../../components/ui";

const PAGE_SIZE = 20;
const REGISTRATION_QUERY_PREFIX = ["admin", "user-registrations"] as const;

type ReviewNotice = { kind: "success" | "error"; text: string };

export function AdminUserRegistrationsPanel() {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [notice, setNotice] = useState<ReviewNotice | null>(null);
  const registrations = useQuery({
    queryKey: [...REGISTRATION_QUERY_PREFIX, page],
    queryFn: () => api.adminUserRegistrations({ page, pageSize: PAGE_SIZE }),
    retry: false,
  });
  const libraries = useQuery({
    queryKey: queryKeys.adminLibraries,
    queryFn: () => api.adminLibraries(),
    retry: false,
  });

  const refreshReviewCaches = async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: REGISTRATION_QUERY_PREFIX }),
      queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers }),
    ]);
  };
  const approve = useMutation({
    mutationFn: ({ requestId, permissions }: { requestId: string; permissions: NewUserPermissions }) =>
      api.approveAdminUserRegistration(requestId, { permissions }),
    onSuccess: async () => {
      setNotice({ kind: "success", text: "注册申请已批准，用户已创建。" });
      await refreshReviewCaches();
    },
    onError: (error) => setNotice({ kind: "error", text: safeReviewError(error, "审批") }),
  });
  const reject = useMutation({
    mutationFn: (requestId: string) => api.rejectAdminUserRegistration(requestId),
    onSuccess: async () => {
      setNotice({ kind: "success", text: "注册申请已拒绝。" });
      await refreshReviewCaches();
    },
    onError: (error) => setNotice({ kind: "error", text: safeReviewError(error, "拒绝") }),
  });

  const requests = registrations.data?.requests ?? [];
  const allLibraries = libraries.data?.libraries ?? [];
  const totalCount = registrations.data?.totalCount ?? 0;
  const pageCount = Math.max(1, Math.ceil(totalCount / PAGE_SIZE));
  const busy = registrations.isFetching || approve.isPending || reject.isPending;

  useEffect(() => {
    if (registrations.data && page > pageCount) setPage(pageCount);
  }, [page, pageCount, registrations.data]);

  async function approveRequest(request: AdminUserRegistration, permissions: NewUserPermissions) {
    const summary = describePermissions(permissions, allLibraries);
    if (!(await confirm(`确认批准 ${request.recipientEmail} 并创建普通用户吗？\n${summary}`))) return;
    setNotice(null);
    approve.mutate({ requestId: request.id, permissions });
  }

  async function rejectRequest(request: AdminUserRegistration) {
    if (!(await confirm(`确认拒绝 ${request.recipientEmail} 的注册申请吗？此操作无法撤销。`))) return;
    setNotice(null);
    reject.mutate(request.id);
  }

  return (
    <section className="lux-admin-panel lux-admin-user-registration-panel" aria-labelledby="lux-admin-user-registration-title">
      <div className="lux-admin-panel-heading">
        <div>
          <span className="lux-admin-eyebrow">公开注册</span>
          <h2 id="lux-admin-user-registration-title">待审批申请</h2>
          <p>仅列出已验证邮箱的申请。批准时会按下方权限创建普通用户。</p>
        </div>
        <span className="lux-admin-user-registration-count">共 {totalCount} 项</span>
      </div>

      {notice ? <p className={notice.kind === "error" ? "lux-error-copy" : "lux-admin-user-registration-notice"} role={notice.kind === "error" ? "alert" : "status"}>{notice.text}</p> : null}

      {registrations.isPending ? <p className="lux-admin-muted" role="status">正在读取待审批申请…</p> : null}
      {registrations.error ? (
        <div className="lux-admin-user-registration-error" role="alert">
          <p>无法读取待审批申请，请稍后重试。</p>
          <LuxButton tone="secondary" size="compact" type="button" disabled={registrations.isFetching} onClick={() => void registrations.refetch()}>
            {registrations.isFetching ? "正在重试…" : "重新加载申请"}
          </LuxButton>
        </div>
      ) : null}
      {libraries.isPending && requests.some((request) => request.permissions.libraryAccessMode === "SELECTED") ? (
        <p className="lux-admin-muted" role="status">正在读取可分配媒体库…</p>
      ) : null}
      {libraries.error && requests.some((request) => request.permissions.libraryAccessMode === "SELECTED") ? (
        <div className="lux-admin-user-registration-error" role="alert">
          <p>媒体库信息暂不可用；指定媒体库权限暂不能批准。</p>
          <LuxButton tone="secondary" size="compact" type="button" disabled={libraries.isFetching} onClick={() => void libraries.refetch()}>
            {libraries.isFetching ? "正在重试…" : "重新加载媒体库"}
          </LuxButton>
        </div>
      ) : null}

      {!registrations.isPending && !registrations.error && totalCount === 0 ? (
        <p className="lux-admin-muted" role="status">目前没有待审批申请。</p>
      ) : null}
      {!registrations.isPending && !registrations.error && totalCount > 0 && requests.length === 0 ? (
        <p className="lux-admin-muted" role="status">此页没有待审批申请。</p>
      ) : null}

      {requests.length > 0 ? (
        <div className="lux-admin-user-registration-list" aria-label="待审批申请列表">
          {requests.map((request) => (
            <RegistrationReviewCard
              key={request.id}
              request={request}
              libraries={allLibraries}
              librariesUnavailable={libraries.isPending || libraries.isError}
              busy={busy}
              onApprove={approveRequest}
              onReject={rejectRequest}
            />
          ))}
        </div>
      ) : null}

      <nav className="lux-admin-user-session-pagination" aria-label="待审批分页">
        <span>第 {page} / {pageCount} 页</span>
        <div>
          <LuxButton tone="secondary" size="compact" type="button" aria-label="待审批上一页" disabled={page <= 1 || busy} onClick={() => setPage((current) => Math.max(1, current - 1))}>上一页</LuxButton>
          <LuxButton tone="secondary" size="compact" type="button" aria-label="待审批下一页" disabled={page >= pageCount || busy} onClick={() => setPage((current) => Math.min(pageCount, current + 1))}>下一页</LuxButton>
        </div>
      </nav>
    </section>
  );
}

function RegistrationReviewCard({
  request,
  libraries,
  librariesUnavailable,
  busy,
  onApprove,
  onReject,
}: {
  request: AdminUserRegistration;
  libraries: AdminLibrary[];
  librariesUnavailable: boolean;
  busy: boolean;
  onApprove: (request: AdminUserRegistration, permissions: NewUserPermissions) => void;
  onReject: (request: AdminUserRegistration) => void;
}) {
  const [permissions, setPermissions] = useState<NewUserPermissions>(() => ({
    ...request.permissions,
    libraryIds: [...request.permissions.libraryIds],
  }));
  const libraryOptions = mergeLibraryOptions(libraries, request.permissions.libraryIds);
  const enabledLibraryIds = new Set(libraries.filter((library) => library.isEnabled).map((library) => library.id));
  const hasUnavailableSelection = permissions.libraryAccessMode === "SELECTED"
    && permissions.libraryIds.some((libraryId) => !enabledLibraryIds.has(libraryId));
  const approvalBlocked = busy || (permissions.libraryAccessMode === "SELECTED" && (librariesUnavailable || hasUnavailableSelection));

  function changeLibraryMode(libraryAccessMode: AdminUserLibraryAccessMode) {
    setPermissions((current) => ({
      ...current,
      libraryAccessMode,
      libraryIds: libraryAccessMode === "SELECTED" ? current.libraryIds : [],
    }));
  }

  function changeLibrary(libraryId: string, selected: boolean) {
    setPermissions((current) => ({
      ...current,
      libraryIds: selected
        ? [...new Set([...current.libraryIds, libraryId])]
        : current.libraryIds.filter((id) => id !== libraryId),
    }));
  }

  function changePlaybackLimit(value: string) {
    if (value === "") {
      setPermissions((current) => ({ ...current, maxSimultaneousPlaybacks: 0 }));
      return;
    }
    const parsed = Number(value);
    if (Number.isSafeInteger(parsed) && parsed >= 0) {
      setPermissions((current) => ({ ...current, maxSimultaneousPlaybacks: parsed }));
    }
  }

  return (
    <article className="lux-admin-user-registration-card" aria-labelledby={`registration-${request.id}-title`}>
      <header className="lux-admin-user-registration-card-heading">
        <div>
          <h3 id={`registration-${request.id}-title`}>{request.recipientEmail}</h3>
          <p>提交于 <time dateTime={safeDateTime(request.createdAt)}>{formatTimestamp(request.createdAt)}</time> · 邮箱验证于 <time dateTime={safeDateTime(request.emailVerifiedAt)}>{formatTimestamp(request.emailVerifiedAt)}</time></p>
        </div>
        <span className="lux-admin-user-registration-verified">邮箱已验证</span>
      </header>

      <div className="lux-admin-user-registration-snapshot">
        <strong>申请时权限快照</strong>
        <p>{describePermissions(request.permissions, libraries)}</p>
      </div>

      <div className="lux-admin-user-registration-editor" aria-label={`拟分配权限 ${request.recipientEmail}`}>
        <h4>批准后分配的权限</h4>
        <label className="lux-admin-user-registration-field">
          媒体库访问范围
          <LuxSelectField aria-label={`媒体库访问范围 ${request.recipientEmail}`} value={permissions.libraryAccessMode} disabled={busy} onChange={(event) => changeLibraryMode(event.target.value as AdminUserLibraryAccessMode)}>
            <option value="ALL">所有已启用媒体库</option>
            <option value="SELECTED">指定媒体库</option>
            <option value="NONE">无媒体库访问权限</option>
          </LuxSelectField>
        </label>
        {permissions.libraryAccessMode === "SELECTED" ? (
          <fieldset className="lux-admin-user-registration-libraries">
            <legend>指定媒体库</legend>
            {libraryOptions.length ? libraryOptions.map((library) => (
              <LuxCheckbox aria-label={`媒体库 ${library.name} ${request.recipientEmail}`} checked={permissions.libraryIds.includes(library.id)} disabled={busy || librariesUnavailable} onChange={(event) => changeLibrary(library.id, event.target.checked)} label=<>{library.name}{library.unavailable ? "（当前不可用）" : ""}</> />
            )) : <p className="lux-admin-muted">暂无可选媒体库。</p>}
          </fieldset>
        ) : null}
        <fieldset className="lux-admin-user-registration-options">
          <legend>播放与文件权限</legend>
          <LuxCheckbox aria-label={`允许播放 ${request.recipientEmail}`} checked={permissions.canPlay} disabled={busy} onChange={(event) => setPermissions((current) => ({ ...current, canPlay: event.target.checked }))} label="允许播放" />
          <LuxCheckbox aria-label={`允许下载 ${request.recipientEmail}`} checked={permissions.canDownload} disabled={busy} onChange={(event) => setPermissions((current) => ({ ...current, canDownload: event.target.checked }))} label="允许下载" />
          <LuxCheckbox aria-label={`允许封装转换 ${request.recipientEmail}`} checked={permissions.allowRemux} disabled={busy} onChange={(event) => setPermissions((current) => ({ ...current, allowRemux: event.target.checked }))} label="允许封装转换" />
          <LuxCheckbox aria-label={`允许音频转码 ${request.recipientEmail}`} checked={permissions.allowAudioTranscode} disabled={busy} onChange={(event) => setPermissions((current) => ({ ...current, allowAudioTranscode: event.target.checked }))} label="允许音频转码" />
          <LuxCheckbox aria-label={`允许视频转码 ${request.recipientEmail}`} checked={permissions.allowVideoTranscode} disabled={busy} onChange={(event) => setPermissions((current) => ({ ...current, allowVideoTranscode: event.target.checked }))} label="允许视频转码" />
          <label className="lux-admin-user-registration-field">同时播放上限（0 表示不限）<LuxTextField type="number" min={0} step={1} aria-label={`同时播放上限 ${request.recipientEmail}`} value={permissions.maxSimultaneousPlaybacks} disabled={busy} onChange={(event) => changePlaybackLimit(event.target.value)} /></label>
        </fieldset>
      </div>

      <div className="lux-admin-user-profile-actions">
        <LuxButton tone="primary" size="compact" type="button" aria-label={`批准 ${request.recipientEmail}`} disabled={approvalBlocked} onClick={() => onApprove(request, permissions)}>
          {busy ? "处理中…" : "批准"}
        </LuxButton>
        <LuxButton tone="secondary" size="compact" type="button" aria-label={`拒绝 ${request.recipientEmail}`} disabled={busy} onClick={() => onReject(request)}>
          {busy ? "处理中…" : "拒绝"}
        </LuxButton>
      </div>
    </article>
  );
}

function mergeLibraryOptions(libraries: AdminLibrary[], snapshotLibraryIds: string[]) {
  const byId = new Map(libraries.filter((library) => library.isEnabled)
    .map((library) => [library.id, { id: library.id, name: library.name, unavailable: false }]));
  for (const id of snapshotLibraryIds) {
    if (!byId.has(id)) {
      const knownLibrary = libraries.find((library) => library.id === id);
      byId.set(id, { id, name: knownLibrary?.name ?? id, unavailable: true });
    }
  }
  return [...byId.values()];
}

function describeLibraryAccess(
  permissions: NewUserPermissions,
  libraries: Array<Pick<AdminLibrary, "id" | "name">>,
) {
  if (permissions.libraryAccessMode === "ALL") return "所有已启用媒体库";
  if (permissions.libraryAccessMode === "NONE") return "无媒体库访问权限";
  const names = permissions.libraryIds.map((id) => libraries.find((library) => library.id === id)?.name ?? id);
  return names.length ? names.join("、") : "未选择媒体库";
}

function describePermissions(
  permissions: NewUserPermissions,
  libraries: Array<Pick<AdminLibrary, "id" | "name">>,
) {
  const playbackLimit = permissions.maxSimultaneousPlaybacks === 0
    ? "不限"
    : `最多同时播放 ${permissions.maxSimultaneousPlaybacks} 路`;
  const yesNo = (enabled: boolean) => enabled ? "允许" : "禁止";
  return [
    `媒体库：${describeLibraryAccess(permissions, libraries)}`,
    `播放：${yesNo(permissions.canPlay)}`,
    `同时播放：${playbackLimit}`,
    `下载：${yesNo(permissions.canDownload)}`,
    `封装转换：${yesNo(permissions.allowRemux)}`,
    `音频转码：${yesNo(permissions.allowAudioTranscode)}`,
    `视频转码：${yesNo(permissions.allowVideoTranscode)}`,
  ].join("；");
}

function safeReviewError(error: unknown, action: "审批" | "拒绝") {
  if (error instanceof ApiError) {
    if (error.status === 409) return "申请状态已变化，请刷新列表后重试。";
    if (error.status === 400) return "申请权限无效或媒体库已不可用，请检查后重试。";
    if (error.status === 401 || error.status === 403) return "管理员会话已失效或权限不足，请重新登录。";
    if (error.status === 503) return "审核服务暂不可用，请稍后重试。";
  }
  return `${action}失败，申请仍保留。请稍后重试。`;
}

function safeDateTime(timestamp: number) {
  const date = new Date(timestamp * 1000);
  return Number.isFinite(timestamp) && !Number.isNaN(date.valueOf()) ? date.toISOString() : "";
}

function formatTimestamp(timestamp: number) {
  const date = new Date(timestamp * 1000);
  return Number.isFinite(timestamp) && !Number.isNaN(date.valueOf())
    ? date.toLocaleString("zh-CN")
    : "时间未知";
}
