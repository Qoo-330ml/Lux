import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useEffect, useState } from "react";
import { api } from "../../../../lib/api/client";
import { queryKeys } from "../../../../lib/api/query-keys";
import type { AdminSettings, AdminUserLibraryAccessMode, NewUserPermissions } from "../../../../lib/api/types";
import { LuxButton, LuxCheckbox, LuxRadio, LuxTextField } from "../../../../components/ui";
import { AdminUserManagementState } from "../AdminUserManagementState";

export function AdminUserDefaultPermissionsSettings() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const librariesQuery = useQuery({ queryKey: queryKeys.adminLibraries, queryFn: () => api.adminLibraries() });
  const [draft, setDraft] = useState<NewUserPermissions | null>(null);
  const [limitDraft, setLimitDraft] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const current = settings.data?.newUserPermissions;
  const currentDraft = draft ?? current;
  const libraries = librariesQuery.data?.libraries ?? [];
  const enabledLibraries = libraries.filter((library) => library.isEnabled);
  const enabledLibraryIds = new Set(enabledLibraries.map((library) => library.id));
  const selectedLibraryIds = currentDraft?.libraryIds.filter((id) => enabledLibraryIds.has(id)) ?? [];
  const unavailableLibraryNames = currentDraft?.libraryIds
    .filter((id) => !enabledLibraryIds.has(id))
    .map((id) => libraries.find((library) => library.id === id)?.name ?? id) ?? [];
  const modeForSave: AdminUserLibraryAccessMode = currentDraft?.libraryAccessMode === "SELECTED" && selectedLibraryIds.length === 0
    ? "NONE"
    : currentDraft?.libraryAccessMode ?? "NONE";
  const libraryIdsForSave = modeForSave === "SELECTED" ? selectedLibraryIds : [];
  const limitInput = limitDraft ?? (currentDraft ? String(currentDraft.maxSimultaneousPlaybacks) : "");
  const parsedLimit = Number(limitInput);
  const validLimit = limitInput.trim() !== "" && Number.isSafeInteger(parsedLimit) && parsedLimit >= 0;
  const updatedDraft = currentDraft && validLimit ? {
    ...currentDraft,
    libraryAccessMode: modeForSave,
    libraryIds: libraryIdsForSave,
    maxSimultaneousPlaybacks: parsedLimit,
  } : null;
  const hasChanges = current != null && updatedDraft != null && !sameNewUserPermissions(updatedDraft, current);
  const update = useMutation({
    mutationFn: (permissions: NewUserPermissions) => api.updateAdminSettings({ newUserPermissions: permissions }),
    onSuccess: (updated, submitted) => {
      const saved = updated.newUserPermissions ?? submitted;
      queryClient.setQueryData<AdminSettings>(queryKeys.adminSettings, (previous) => ({
        ...previous,
        ...updated,
        newUserPermissions: saved,
      } as AdminSettings));
      setDraft(saved);
      setLimitDraft(String(saved.maxSimultaneousPlaybacks));
      setError("");
      setNotice("新用户默认权限已保存。");
    },
    onError: (cause) => {
      setNotice("");
      setError(cause.message || "新用户默认权限保存失败，请重试。");
    },
  });

  useEffect(() => {
    if (!current) return;
    setDraft(current);
    setLimitDraft(String(current.maxSimultaneousPlaybacks));
  }, [current]);

  if (settings.isPending || librariesQuery.isPending) {
    return <AdminUserManagementState label="正在读取新用户默认权限…" />;
  }
  if (settings.error) {
    return <AdminUserManagementState label={`读取新用户默认权限失败：${settings.error.message}`} error />;
  }
  if (librariesQuery.error) {
    return <AdminUserManagementState label={`读取媒体库列表失败：${librariesQuery.error.message}`} error />;
  }
  if (!current || !currentDraft) {
    return <AdminUserManagementState label="服务端没有返回新用户默认权限，未展示本地默认值。" error />;
  }

  function updateDraft(change: Partial<NewUserPermissions>) {
    if (!currentDraft) return;
    setDraft({ ...currentDraft, ...change });
    setNotice("");
    setError("");
  }

  function toggleLibrary(libraryId: string, checked: boolean) {
    const next = new Set(selectedLibraryIds);
    if (checked) next.add(libraryId);
    else next.delete(libraryId);
    updateDraft({ libraryIds: [...next] });
  }

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!updatedDraft || !validLimit || !hasChanges || update.isPending) return;
    setNotice("");
    setError("");
    update.mutate(updatedDraft);
  }

  return (
    <section className="lux-admin-panel lux-admin-user-permissions lux-admin-new-user-permissions" aria-labelledby="lux-admin-new-user-permissions-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-new-user-permissions-title">新用户默认权限</h2></div></div>
      <form className="lux-admin-form lux-admin-user-defaults-form" onSubmit={submit}>
        <fieldset className="lux-admin-user-access-modes">
          <legend>媒体库访问范围</legend>
          {([
            ["ALL", "全部媒体库", "允许访问所有已启用媒体库，包括之后新增的媒体库。"],
            ["SELECTED", "指定媒体库", "仅允许访问下方勾选的媒体库。"],
            ["NONE", "无媒体库权限", "用户不能浏览、搜索或播放任何媒体库内容。"],
          ] as const).map(([mode, title, description]) => (
            <LuxRadio key={mode} name="new-user-library-access" value={mode} checked={currentDraft.libraryAccessMode === mode} disabled={update.isPending} onChange={() => updateDraft({ libraryAccessMode: mode })} label=<><strong>{title}</strong><small>{description}</small></> />
          ))}
        </fieldset>
        {currentDraft.libraryAccessMode === "ALL" ? <p className="lux-admin-muted">新用户可访问所有已启用媒体库，包括之后新增的媒体库。</p> : null}
        {currentDraft.libraryAccessMode === "NONE" ? <p className="lux-admin-muted">新用户不能访问任何媒体库。</p> : null}
        {currentDraft.libraryAccessMode === "SELECTED" ? (
          <>
            {unavailableLibraryNames.length ? <p className="lux-admin-muted">已停用或不可用媒体库将从默认权限移除：{unavailableLibraryNames.join("、")}。</p> : null}
            <p className="lux-admin-muted">
              {enabledLibraries.length
                ? selectedLibraryIds.length
                  ? `已选择 ${selectedLibraryIds.length} 个媒体库。`
                  : "请至少选择一个媒体库；空选择保存后会设为无媒体库权限。"
                : "当前没有已启用的媒体库；空选择保存后会设为无媒体库权限。"}
            </p>
            {enabledLibraries.length ? (
              <div className="lux-admin-library-access">
                {enabledLibraries.map((library) => (
                  <LuxCheckbox key={library.id} value={library.id} checked={selectedLibraryIds.includes(library.id)} disabled={update.isPending} onChange={(event) => toggleLibrary(library.id, event.currentTarget.checked)} label=<>{library.name}</> />
                ))}
              </div>
            ) : null}
          </>
        ) : null}
        <fieldset className="lux-admin-user-defaults-policy-options">
          <legend>下载与播放/转码</legend>
          <LuxCheckbox aria-label="允许下载" checked={currentDraft.canDownload} disabled={update.isPending} onChange={(event) => updateDraft({ canDownload: event.currentTarget.checked })} label="允许下载" />
          <LuxCheckbox aria-label="允许播放" checked={currentDraft.canPlay} disabled={update.isPending} onChange={(event) => updateDraft({ canPlay: event.currentTarget.checked })} label="允许播放" />
          <LuxCheckbox aria-label="允许封装转换" checked={currentDraft.allowRemux} disabled={update.isPending} onChange={(event) => updateDraft({ allowRemux: event.currentTarget.checked })} label="允许封装转换" />
          <LuxCheckbox aria-label="允许音频转码" checked={currentDraft.allowAudioTranscode} disabled={update.isPending} onChange={(event) => updateDraft({ allowAudioTranscode: event.currentTarget.checked })} label="允许音频转码" />
          <LuxCheckbox aria-label="允许视频转码" checked={currentDraft.allowVideoTranscode} disabled={update.isPending} onChange={(event) => updateDraft({ allowVideoTranscode: event.currentTarget.checked })} label="允许视频转码" />
        </fieldset>
        <label className="lux-admin-user-playback-limit">
          同时播放上限（0 表示不限）
          <LuxTextField type="number" aria-label="新用户同时播放上限" min={0} step={1} value={limitInput} disabled={update.isPending} onChange={(event) => { setLimitDraft(event.currentTarget.value); setNotice(""); setError(""); }} />
        </label>
        {!validLimit ? <p className="lux-admin-user-playback-validation">请输入大于或等于 0 的整数。</p> : null}
        <p className="lux-admin-muted">此处配置新账号的初始权限快照；修改不会自动改变已有用户。</p>
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="primary" type="submit" disabled={!validLimit || !hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存默认权限"}
          </LuxButton>
          {error ? <span role="alert">{error}</span> : notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}

function sameNewUserPermissions(left: NewUserPermissions, right: NewUserPermissions) {
  return left.libraryAccessMode === right.libraryAccessMode
    && left.libraryIds.length === right.libraryIds.length
    && left.libraryIds.every((id, index) => id === right.libraryIds[index])
    && left.canDownload === right.canDownload
    && left.canPlay === right.canPlay
    && left.maxSimultaneousPlaybacks === right.maxSimultaneousPlaybacks
    && left.allowRemux === right.allowRemux
    && left.allowAudioTranscode === right.allowAudioTranscode
    && left.allowVideoTranscode === right.allowVideoTranscode;
}
