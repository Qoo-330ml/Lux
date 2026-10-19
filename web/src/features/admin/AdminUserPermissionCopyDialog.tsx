import { useMutation, useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { api } from "../../lib/api/client";
import type {
  AdminUser,
  AdminUserPermissionCopyGroups,
  AdminUserPermissionCopyPreview,
  AdminUserPermissionCopyPreviewInput,
} from "../../lib/api/types";

type PermissionCopyTarget = {
  id: string;
  name: string;
};

type Props = {
  targets: PermissionCopyTarget[];
  onClose: () => void;
  onApplied: () => void;
};

const DEFAULT_GROUPS: AdminUserPermissionCopyGroups = {
  library: true,
  content: true,
  playback: true,
};

export function AdminUserPermissionCopyDialog({ targets, onClose, onApplied }: Props) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const [sourceSearchInput, setSourceSearchInput] = useState("");
  const [sourceSearch, setSourceSearch] = useState("");
  const [sourceUserId, setSourceUserId] = useState("");
  const [groups, setGroups] = useState(DEFAULT_GROUPS);
  const [results, setResults] = useState<Array<{ userId: string; name: string; success: boolean; errorCode: string | null }> | null>(null);
  const [copyError, setCopyError] = useState("");
  const targetSnapshot = useState(() => targets)[0];
  const targetIds = targetSnapshot.map((target) => target.id);
  const sourceUsers = useQuery({
    queryKey: ["admin", "users", "permission-copy-source", sourceSearch],
    queryFn: () => api.adminUsers({
      page: 1,
      pageSize: 100,
      search: sourceSearch,
      status: "all",
      sortBy: "createdAt",
      sortOrder: "desc",
    }),
  });
  const libraries = useQuery({ queryKey: ["admin", "libraries"], queryFn: () => api.adminLibraries() });
  const libraryNames = new Map((libraries.data?.libraries ?? []).map((library) => [library.id, library.name]));
  const candidates = (sourceUsers.data?.users ?? []).filter((user) => !targetIds.includes(user.id));
  const preview = useMutation({ mutationFn: (input: AdminUserPermissionCopyPreviewInput) => api.previewAdminUserPermissionCopy(input) });
  const apply = useMutation({
    mutationFn: () => {
      if (!preview.data) throw new Error("请先预览权限复制内容");
      return api.copyAdminUserPermissionGroups({
        sourceUserId: preview.data.sourceUserId,
        sourceVersion: preview.data.sourceVersion,
        targets: preview.data.targets.map(({ userId, expectedVersion }) => ({ userId, expectedVersion })),
        groups: preview.data.groups,
      });
    },
    onSuccess: (response) => {
      const names = new Map(targetSnapshot.map((target) => [target.id, target.name]));
      setResults(response.results.map((result) => ({
        ...result,
        name: names.get(result.userId) ?? result.userId,
      })));
      setCopyError("");
      onApplied();
    },
    onError: (error) => {
      setCopyError(error.message);
      preview.reset();
    },
  });

  useEffect(() => {
    const timer = window.setTimeout(() => setSourceSearch(sourceSearchInput.trim()), 250);
    return () => window.clearTimeout(timer);
  }, [sourceSearchInput]);

  useEffect(() => {
    dialogRef.current?.querySelector<HTMLButtonElement>("[data-dialog-close]")?.focus();
  }, []);

  function resetPreview() {
    preview.reset();
    apply.reset();
    setResults(null);
    setCopyError("");
  }

  function changeSource(userId: string) {
    setSourceUserId(userId);
    resetPreview();
  }

  function changeGroup(group: keyof AdminUserPermissionCopyGroups, checked: boolean) {
    setGroups((current) => ({ ...current, [group]: checked }));
    resetPreview();
  }

  function previewCopy() {
    if (!selectedSourceIsValid || !targetSnapshot.length || !Object.values(groups).some(Boolean) || preview.isPending || apply.isPending) return;
    setResults(null);
    setCopyError("");
    apply.reset();
    preview.reset();
    preview.mutate({ sourceUserId, targetUserIds: targetIds, groups });
  }

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape" && !apply.isPending) {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
      'button:not(:disabled), input:not(:disabled), select:not(:disabled), [tabindex="0"]',
    );
    if (!focusable?.length) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  const selectedGroups = [
    groups.library ? "媒体库" : null,
    groups.content ? "内容限制" : null,
    groups.playback ? "播放与下载" : null,
  ].filter((group): group is string => group !== null);
  const selectedSourceIsValid = sourceUserId.length > 0 && !targetIds.includes(sourceUserId);
  const copyDisabled = apply.isPending || preview.isPending || !preview.data || !selectedSourceIsValid || results !== null;

  return (
    <div className="lux-admin-user-permission-copy-backdrop">
      <div
        ref={dialogRef}
        className="lux-admin-user-permission-copy"
        role="dialog"
        aria-modal="true"
        aria-label="权限复制"
        tabIndex={-1}
        onKeyDown={handleKeyDown}
      >
        <header className="lux-admin-user-permission-copy-heading">
          <div>
            <span className="lux-admin-eyebrow">批量配置</span>
            <h2>预览并复制权限</h2>
            <p>只复制所选权限组；服务器管理员角色、账号资料、凭证和偏好不会复制。</p>
          </div>
          <button className="lux-icon-button lux-icon-button-small" type="button" data-dialog-close aria-label="关闭权限复制" onClick={onClose} disabled={apply.isPending}>×</button>
        </header>

        <div className="lux-admin-user-permission-copy-targets">
          <strong>目标账号 · {targetSnapshot.length} 人</strong>
          <ul>{targetSnapshot.map((target) => <li key={target.id}>{target.name}</li>)}</ul>
        </div>

        <div className="lux-admin-user-permission-copy-form">
          <label>
            <span>搜索来源账号</span>
            <input type="search" value={sourceSearchInput} onChange={(event) => setSourceSearchInput(event.target.value)} placeholder="用户名、显示名称或邮箱" disabled={preview.isPending || apply.isPending} />
          </label>
          {sourceUsers.isPending ? <p role="status">正在查找来源账号…</p> : null}
          {sourceUsers.error ? <p className="lux-error-copy" role="alert">来源账号加载失败：{sourceUsers.error.message}</p> : null}
          {!sourceUsers.isPending && !sourceUsers.error && candidates.length === 0 ? <p role="status">没有可用的来源账号。请调整搜索条件；已选目标不能作为来源。</p> : null}
          {candidates.length > 0 ? (
            <label>
              <span>来源账号</span>
              <select aria-label="权限复制来源" value={sourceUserId} onChange={(event) => changeSource(event.target.value)} disabled={preview.isPending || apply.isPending}>
                <option value="">选择来源账号</option>
                {candidates.map((user) => <option key={user.id} value={user.id}>{adminUserName(user)}{user.isDisabled ? " · 已禁用" : ""}</option>)}
              </select>
            </label>
          ) : null}
          <fieldset disabled={preview.isPending || apply.isPending}>
            <legend>复制权限组</legend>
            <label><input aria-label="复制媒体库权限" type="checkbox" checked={groups.library} onChange={(event) => changeGroup("library", event.target.checked)} />媒体库访问</label>
            <label><input aria-label="复制内容限制" type="checkbox" checked={groups.content} onChange={(event) => changeGroup("content", event.target.checked)} />内容分级与标签限制</label>
            <label><input aria-label="复制播放权限" type="checkbox" checked={groups.playback} onChange={(event) => changeGroup("playback", event.target.checked)} />播放、下载与转码</label>
          </fieldset>
          <p className="lux-admin-muted">至少选择一个权限组；每次最多复制到 100 个账号。</p>
          <button className="lux-button lux-button-primary" type="button" aria-label="预览权限复制" onClick={previewCopy} disabled={!selectedSourceIsValid || !selectedGroups.length || preview.isPending || apply.isPending}>
            {preview.isPending ? "正在生成预览…" : "预览所选权限"}
          </button>
        </div>

        {preview.error ? <p className="lux-error-copy" role="alert">预览失败：{preview.error.message}</p> : null}
        {preview.data ? (
          <section className="lux-admin-user-permission-copy-preview" aria-label="权限复制预览">
            <h3>复制预览</h3>
            <p>来源版本 {preview.data.sourceVersion} · 权限组：{selectedGroups.join("、")}</p>
            <PermissionSnapshot snapshot={preview.data.sourceSnapshot} groups={preview.data.groups} libraryNames={libraryNames} />
            <ul className="lux-admin-user-permission-copy-versions">
              {preview.data.targets.map((target) => (
                <li key={target.userId}>
                  <span>{targetSnapshot.find((item) => item.id === target.userId)?.name ?? target.userId}</span>
                  <span>预览版本 {target.expectedVersion}</span>
                </li>
              ))}
            </ul>
            <p className="lux-admin-muted">应用前会再次检查来源和目标版本；发生变化的来源会阻止整批写入，目标冲突会单独报告。</p>
            <button className="lux-button lux-button-primary" type="button" aria-label="应用权限复制" onClick={() => apply.mutate()} disabled={copyDisabled}>
              {apply.isPending ? "正在复制…" : `应用到 ${preview.data.targets.length} 个账号`}
            </button>
          </section>
        ) : null}
        {copyError ? <p className="lux-error-copy" role="alert">复制失败：{copyError}。请重新预览后重试。</p> : null}
        {results ? (
          <section className="lux-admin-user-permission-copy-results" role="status" aria-label="权限复制结果">
            <h3>复制结果</h3>
            <ul>{results.map((result) => <li key={result.userId} className={result.success ? "is-success" : "is-failure"}>{result.name}：{result.success ? "成功" : copyErrorMessage(result.errorCode)}</li>)}</ul>
          </section>
        ) : null}
        <footer className="lux-admin-user-permission-copy-footer">
          <button className="lux-button lux-button-secondary" type="button" onClick={onClose} disabled={apply.isPending}>关闭</button>
        </footer>
      </div>
    </div>
  );
}

function PermissionSnapshot({ snapshot, groups, libraryNames }: {
  snapshot: AdminUserPermissionCopyPreview["sourceSnapshot"];
  groups: AdminUserPermissionCopyGroups;
  libraryNames: Map<string, string>;
}) {
  return (
    <dl className="lux-admin-user-permission-copy-snapshot">
      {groups.library && snapshot.library ? <><dt>媒体库</dt><dd>{libraryAccessLabel(snapshot.library.accessMode, snapshot.library.libraryIds, libraryNames)}</dd></> : null}
      {groups.content ? <><dt>内容限制</dt><dd>{contentPolicyLabel(snapshot.content)}</dd></> : null}
      {groups.playback && snapshot.playback ? <><dt>播放权限</dt><dd>{snapshot.playback.canPlay ? "允许播放" : "禁止播放"} · 下载 {snapshot.playback.canDownload ? "允许" : "禁止"} · 最大同时播放 {snapshot.playback.maxSimultaneousPlaybacks === 0 ? "不限" : `${snapshot.playback.maxSimultaneousPlaybacks} 路`} · Remux {snapshot.playback.allowRemux ? "允许" : "禁止"} · 音频转码 {snapshot.playback.allowAudioTranscode ? "允许" : "禁止"} · 视频转码 {snapshot.playback.allowVideoTranscode ? "允许" : "禁止"}</dd></> : null}
    </dl>
  );
}

function libraryAccessLabel(mode: string, libraryIds: string[], libraryNames: Map<string, string>) {
  if (mode === "ALL") return "全部媒体库";
  if (mode === "NONE") return "无媒体库权限";
  if (!libraryIds.length) return "指定媒体库（当前没有可用项）";
  return `指定：${libraryIds.map((id) => libraryNames.get(id) ?? id).join("、")}`;
}

function contentPolicyLabel(policy: AdminUserPermissionCopyPreview["sourceSnapshot"]["content"]) {
  if (!policy) return "未设置内容限制";
  const maximumRating = policy.maximumRating ?? "不限制最高等级";
  const allowedTags = policy.allowedTags.length ? policy.allowedTags.join("、") : "未限制";
  const blockedTags = policy.blockedTags.length ? policy.blockedTags.join("、") : "无";
  return `${policy.ratingSystem} · 最高 ${maximumRating} · 未分级内容${policy.allowUnrated ? "允许" : "禁止"} · 允许标签 ${allowedTags} · 禁止标签 ${blockedTags}`;
}

function adminUserName(user: AdminUser) {
  const name = user.displayName || user.usernameNormalized;
  return user.email ? `${name} · ${user.email}` : `${name} · ${user.usernameNormalized}`;
}

function copyErrorMessage(code: string | null) {
  switch (code) {
    case "USER_UNAVAILABLE": return "账号不存在或已删除";
    case "VERSION_CONFLICT": return "权限已变化，请重新预览";
    case "INVALID_SNAPSHOT": return "来源权限当前无效";
    case "STORAGE_UNAVAILABLE": return "服务暂不可用";
    default: return "复制失败";
  }
}
