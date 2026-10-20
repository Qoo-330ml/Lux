import { useMutation, useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { api } from "../../lib/api/client";
import type {
  AdminAccountPreferenceCopyGroups,
  AdminAccountPreferenceCopyPreview,
  AdminAccountPreferenceCopyPreviewInput,
  AdminUser,
} from "../../lib/api/types";

type PreferenceCopyTarget = {
  id: string;
  name: string;
};

type Props = {
  targets: PreferenceCopyTarget[];
  onClose: () => void;
  onApplied: () => void;
};

const DEFAULT_GROUPS: AdminAccountPreferenceCopyGroups = {
  appearance: true,
  home: true,
  playback: true,
  libraryOrder: true,
};

export function AdminUserPreferenceCopyDialog({ targets, onClose, onApplied }: Props) {
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
    queryKey: ["admin", "users", "preference-copy-source", sourceSearch],
    queryFn: () => api.adminUsers({
      page: 1,
      pageSize: 100,
      search: sourceSearch,
      status: "all",
      sortBy: "createdAt",
      sortOrder: "desc",
    }),
  });
  const libraryCatalog = useQuery({
    queryKey: ["admin", "libraries"],
    queryFn: () => api.adminLibraries(),
  });
  const libraryNames = new Map((libraryCatalog.data?.libraries ?? []).map((library) => [library.id, library.name]));
  const candidates = (sourceUsers.data?.users ?? []).filter((user) => !targetIds.includes(user.id));
  const preview = useMutation({ mutationFn: (input: AdminAccountPreferenceCopyPreviewInput) => api.previewAdminAccountPreferencesCopy(input) });
  const apply = useMutation({
    mutationFn: () => {
      if (!preview.data) throw new Error("请先预览复制内容");
      return api.copyAdminAccountPreferences({
        sourceUserId: preview.data.sourceUserId,
        sourceExpectedVersion: preview.data.sourceVersion,
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

  function changeGroup(group: keyof AdminAccountPreferenceCopyGroups, checked: boolean) {
    setGroups((current) => ({ ...current, [group]: checked }));
    resetPreview();
  }

  function previewCopy() {
    if (!sourceUserId || !targetSnapshot.length || !Object.values(groups).some(Boolean) || preview.isPending || apply.isPending) return;
    setResults(null);
    setCopyError("");
    apply.reset();
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
    groups.appearance ? "外观" : null,
    groups.home ? "首页" : null,
    groups.playback ? "播放" : null,
    groups.libraryOrder ? "媒体库排序" : null,
  ].filter((group): group is string => group !== null);
  const selectedSourceIsValid = sourceUserId.length > 0 && !targetIds.includes(sourceUserId);
  const copyDisabled = apply.isPending || preview.isPending || !preview.data || !selectedSourceIsValid || results !== null;

  return (
    <div className="lux-admin-user-preference-copy-backdrop">
      <div
        ref={dialogRef}
        className="lux-admin-user-preference-copy"
        role="dialog"
        aria-modal="true"
        aria-labelledby="lux-admin-user-preference-copy-title"
        tabIndex={-1}
        onKeyDown={handleKeyDown}
      >
        <header className="lux-admin-user-preference-copy-heading">
          <div>
            <span className="lux-admin-eyebrow">批量配置</span>
            <h2 id="lux-admin-user-preference-copy-title">预览并复制账户偏好</h2>
            <p>来源偏好会按预览时的版本复制到所选账号；账号资料、密码、历史记录和收藏不会复制。</p>
          </div>
          <button className="lux-icon-button lux-icon-button-small" type="button" data-dialog-close aria-label="关闭偏好复制" onClick={onClose} disabled={apply.isPending}>×</button>
        </header>

        <div className="lux-admin-user-preference-copy-targets">
          <strong>目标账号 · {targetSnapshot.length} 人</strong>
          <ul>{targetSnapshot.map((target) => <li key={target.id}>{target.name}</li>)}</ul>
        </div>

        <div className="lux-admin-user-preference-copy-form">
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
              <select aria-label="偏好复制来源" value={sourceUserId} onChange={(event) => changeSource(event.target.value)} disabled={preview.isPending || apply.isPending}>
                <option value="">选择来源账号</option>
                {candidates.map((user) => <option key={user.id} value={user.id}>{adminUserName(user)}{user.isDisabled ? " · 已禁用" : ""}</option>)}
              </select>
            </label>
          ) : null}
          <fieldset disabled={preview.isPending || apply.isPending}>
            <legend>复制项目</legend>
            <label><input aria-label="复制外观偏好" type="checkbox" checked={groups.appearance} onChange={(event) => changeGroup("appearance", event.target.checked)} />外观与强调色</label>
            <label><input aria-label="复制首页偏好" type="checkbox" checked={groups.home} onChange={(event) => changeGroup("home", event.target.checked)} />首页分区</label>
            <label><input aria-label="复制播放偏好" type="checkbox" checked={groups.playback} onChange={(event) => changeGroup("playback", event.target.checked)} />音轨、字幕、自动连播、优化图片与已看阈值</label>
            <label><input aria-label="复制媒体库排序" type="checkbox" checked={groups.libraryOrder} onChange={(event) => changeGroup("libraryOrder", event.target.checked)} />媒体库排序</label>
          </fieldset>
          <p className="lux-admin-muted">不选择任何项目时不能继续。每次最多复制到 100 个账号。</p>
          <button className="lux-button lux-button-primary" type="button" aria-label="预览偏好复制" onClick={previewCopy} disabled={!sourceUserId || !selectedGroups.length || preview.isPending || apply.isPending || !selectedSourceIsValid}>
            {preview.isPending ? "正在生成预览…" : "预览所选偏好"}
          </button>
        </div>

        {preview.error ? <p className="lux-error-copy" role="alert">预览失败：{preview.error.message}</p> : null}
        {preview.data ? (
          <section className="lux-admin-user-preference-copy-preview" aria-label="偏好复制预览">
            <h3>复制预览</h3>
            <p>来源版本 {preview.data.sourceVersion} · 复制项目：{selectedGroups.join("、")}</p>
            <PreferenceSnapshot snapshot={preview.data.sourceSnapshot} libraryNames={libraryNames} />
            <ul className="lux-admin-user-preference-copy-versions">
              {preview.data.targets.map((target) => (
                <li key={target.userId}>
                  <span>{targetSnapshot.find((item) => item.id === target.userId)?.name ?? target.userId}</span>
                  <span>当前版本 {target.expectedVersion}</span>
                  {preview.data.groups.libraryOrder ? <small>媒体库排序：{libraryOrderLabel(target.libraryOrder, libraryNames)}</small> : null}
                </li>
              ))}
            </ul>
            <p className="lux-admin-muted">排序预览按目标当前可访问的媒体库过滤；应用时会按目标届时的权限重新过滤，且不会更改管理员强制排序。应用时也会再次检查偏好版本；已发生变化的目标会单独标为冲突。</p>
            <button className="lux-button lux-button-primary" type="button" aria-label="应用偏好复制" onClick={() => apply.mutate()} disabled={copyDisabled}>
              {apply.isPending ? "正在复制…" : `应用到 ${preview.data.targets.length} 个账号`}
            </button>
          </section>
        ) : null}
        {copyError ? <p className="lux-error-copy" role="alert">复制失败：{copyError}。请重新预览后重试。</p> : null}
        {results ? (
          <section className="lux-admin-user-preference-copy-results" role="status" aria-label="偏好复制结果">
            <h3>复制结果</h3>
            <ul>{results.map((result) => <li key={result.userId} className={result.success ? "is-success" : "is-failure"}>{result.name}：{result.success ? "成功" : copyErrorMessage(result.errorCode)}</li>)}</ul>
          </section>
        ) : null}
        <footer className="lux-admin-user-preference-copy-footer">
          <button className="lux-button lux-button-secondary" type="button" onClick={onClose} disabled={apply.isPending}>关闭</button>
        </footer>
      </div>
    </div>
  );
}

function PreferenceSnapshot({ snapshot, libraryNames }: {
  snapshot: AdminAccountPreferenceCopyPreview["sourceSnapshot"];
  libraryNames: Map<string, string>;
}) {
  return (
    <dl className="lux-admin-user-preference-copy-snapshot">
      {snapshot.appearance ? <><dt>外观</dt><dd>{themeName(snapshot.appearance.theme)} · {accentName(snapshot.appearance.accentColor)}</dd></> : null}
      {snapshot.home ? <><dt>首页</dt><dd>{snapshot.home.showMediaLibraries ? "显示媒体库" : "隐藏媒体库"} · {snapshot.home.showContinueWatching ? "显示继续观看" : "隐藏继续观看"}</dd></> : null}
      {snapshot.playback ? <><dt>播放</dt><dd>音轨 {snapshot.playback.audioLanguage} · 字幕 {snapshot.playback.subtitleLanguage} · 自动连播 {snapshot.playback.autoPlayNextEpisode ? "开启" : "关闭"} · 已看阈值 {snapshot.playback.playedPercent}%</dd></> : null}
      {snapshot.libraryOrder ? <><dt>媒体库排序</dt><dd>{libraryOrderLabel(snapshot.libraryOrder, libraryNames)}</dd></> : null}
    </dl>
  );
}

function libraryOrderLabel(libraryIds: string[] | undefined, libraryNames: Map<string, string>) {
  if (!libraryIds?.length) return "没有已保存的媒体库顺序";
  return libraryIds.map((libraryId) => libraryNames.get(libraryId) ?? libraryId).join(" → ");
}

function adminUserName(user: AdminUser) {
  const name = user.displayName || user.usernameNormalized;
  return user.email ? `${name} · ${user.email}` : `${name} · ${user.usernameNormalized}`;
}

function themeName(value: string) {
  return value === "light" ? "浅色主题" : value === "dark" ? "深色主题" : value;
}

function accentName(value: string) {
  const names: Record<string, string> = { silver: "银色", berry: "莓果", ocean: "海洋", amber: "琥珀", mint: "薄荷" };
  return names[value] ?? value;
}

function copyErrorMessage(code: string | null) {
  switch (code) {
    case "USER_UNAVAILABLE": return "账号不存在或已删除";
    case "VERSION_CONFLICT": return "偏好已被其他操作修改";
    case "INVALID_PREFERENCES": return "来源偏好无效";
    case "STORAGE_UNAVAILABLE": return "服务暂不可用";
    default: return "复制失败";
  }
}
