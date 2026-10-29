import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useState } from "react";
import { api } from "../../../../lib/api/client";
import { queryKeys } from "../../../../lib/api/query-keys";
import type { AdminUserLibraryAccessMode, AdminUserPermissionPreset, AdminUserPermissionPresetInput, NewUserPermissions } from "../../../../lib/api/types";
import { LuxButton, LuxCheckbox, LuxRadio, LuxTextField, useLuxConfirm } from "../../../../components/ui";
import { AdminUserManagementState } from "../AdminUserManagementState";

export function AdminUserPermissionPresetsSettings() {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const queryKey = ["admin", "user-permission-presets"] as const;
  const presetsQuery = useQuery({ queryKey, queryFn: () => api.adminUserPermissionPresets() });
  const librariesQuery = useQuery({ queryKey: queryKeys.adminLibraries, queryFn: () => api.adminLibraries() });
  const [editingPreset, setEditingPreset] = useState<AdminUserPermissionPreset | null>(null);
  const [draft, setDraft] = useState<AdminUserPermissionPresetInput | null>(null);
  const [limitDraft, setLimitDraft] = useState("2");
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const presets = presetsQuery.data?.presets ?? [];
  const enabledLibraries = (librariesQuery.data?.libraries ?? []).filter((library) => library.isEnabled);
  const enabledLibraryIds = new Set(enabledLibraries.map((library) => library.id));
  const selectedLibraryIds = draft?.permissions.libraryIds.filter((id) => enabledLibraryIds.has(id)) ?? [];
  const limit = Number(limitDraft);
  const validLimit = limitDraft.trim() !== "" && Number.isSafeInteger(limit) && limit >= 0;
  const validName = Boolean(draft?.name.trim()) && (draft?.name.trim().length ?? 0) <= 80;
  const validDescription = (draft?.description?.length ?? 0) <= 500;
  const modeForSave: AdminUserLibraryAccessMode = draft?.permissions.libraryAccessMode === "SELECTED" && selectedLibraryIds.length === 0
    ? "NONE"
    : draft?.permissions.libraryAccessMode ?? "NONE";
  const canSave = Boolean(draft && validName && validDescription && validLimit);

  const finishMutation = (message: string) => {
    setDraft(null);
    setEditingPreset(null);
    setError("");
    setNotice(message);
    void queryClient.invalidateQueries({ queryKey });
  };
  const showMutationError = (cause: Error) => {
    setNotice("");
    setError(cause.message || "角色预设操作失败，请重试。");
  };
  const create = useMutation({
    mutationFn: (input: AdminUserPermissionPresetInput) => api.createAdminUserPermissionPreset(input),
    onSuccess: () => finishMutation("角色预设已创建。"),
    onError: showMutationError,
  });
  const update = useMutation({
    mutationFn: ({ presetId, version, input }: { presetId: string; version: number; input: AdminUserPermissionPresetInput }) =>
      api.updateAdminUserPermissionPreset(presetId, version, input),
    onSuccess: () => finishMutation("角色预设已更新。"),
    onError: showMutationError,
  });
  const remove = useMutation({
    mutationFn: ({ presetId, version }: { presetId: string; version: number }) =>
      api.deleteAdminUserPermissionPreset(presetId, version),
    onSuccess: () => finishMutation("角色预设已删除。"),
    onError: showMutationError,
  });
  const busy = create.isPending || update.isPending || remove.isPending;

  function startCreate() {
    setEditingPreset(null);
    setDraft({
      name: "",
      description: null,
      isAdmin: false,
      canManageServer: false,
      permissions: {
        libraryAccessMode: "NONE",
        libraryIds: [],
        canDownload: false,
        canPlay: true,
        maxSimultaneousPlaybacks: 2,
        allowRemux: true,
        allowAudioTranscode: true,
        allowVideoTranscode: true,
      },
    });
    setLimitDraft("2");
    setError("");
    setNotice("");
  }

  function startEdit(preset: AdminUserPermissionPreset) {
    setEditingPreset(preset);
    setDraft({
      name: preset.name,
      description: preset.description,
      isAdmin: preset.isAdmin,
      canManageServer: preset.canManageServer,
      permissions: { ...preset.permissions, libraryIds: [...preset.permissions.libraryIds] },
    });
    setLimitDraft(String(preset.permissions.maxSimultaneousPlaybacks));
    setError("");
    setNotice("");
  }

  function updateDraft(change: Partial<AdminUserPermissionPresetInput>) {
    setDraft((current) => current ? {
      ...current,
      ...change,
      canManageServer: change.isAdmin === false ? false : change.canManageServer ?? current.canManageServer,
    } : current);
    setError("");
    setNotice("");
  }

  function updatePermissionDraft(change: Partial<NewUserPermissions>) {
    setDraft((current) => current ? {
      ...current,
      permissions: { ...current.permissions, ...change },
    } : current);
    setError("");
    setNotice("");
  }

  function toggleLibrary(libraryId: string, checked: boolean) {
    const next = new Set(selectedLibraryIds);
    if (checked) next.add(libraryId);
    else next.delete(libraryId);
    updatePermissionDraft({ libraryIds: [...next] });
  }

  function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!draft || !canSave || busy) return;
    const permissions: NewUserPermissions = {
      ...draft.permissions,
      libraryAccessMode: modeForSave,
      libraryIds: modeForSave === "SELECTED" ? selectedLibraryIds : [],
      maxSimultaneousPlaybacks: limit,
    };
    const input: AdminUserPermissionPresetInput = {
      ...draft,
      name: draft.name.trim(),
      description: draft.description?.trim() || null,
      canManageServer: draft.isAdmin && draft.canManageServer,
      permissions,
    };
    setError("");
    setNotice("");
    if (editingPreset) {
      update.mutate({ presetId: editingPreset.id, version: editingPreset.version, input });
    } else {
      create.mutate(input);
    }
  }

  async function deletePreset(preset: AdminUserPermissionPreset) {
    if (busy || preset.isBuiltin) return;
    const accepted = await confirm(`确定删除“${preset.name}”预设吗？已应用到用户的权限快照不会改变。`);
    if (!accepted) return;
    setError("");
    setNotice("");
    remove.mutate({ presetId: preset.id, version: preset.version });
  }

  if (presetsQuery.isPending || librariesQuery.isPending) {
    return <AdminUserManagementState label="正在读取角色预设…" />;
  }
  if (presetsQuery.error) {
    return <AdminUserManagementState label={`读取角色预设失败：${presetsQuery.error.message}`} error />;
  }
  if (librariesQuery.error) {
    return <AdminUserManagementState label={`读取媒体库列表失败：${librariesQuery.error.message}`} error />;
  }

  return (
    <section className="lux-admin-panel lux-admin-user-permissions lux-admin-user-presets" aria-labelledby="lux-admin-user-presets-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-presets-title">角色预设</h2><p className="lux-admin-muted">预设保存权限快照；之后修改不会改变已应用到用户的权限。</p></div>
        <LuxButton tone="primary" type="button" onClick={startCreate} disabled={busy}>新建角色预设</LuxButton>
      </div>
      <div className="lux-admin-user-defaults-form">
        {presets.map((preset) => {
          const librarySummary = preset.permissions.libraryAccessMode === "ALL"
            ? "全部媒体库"
            : preset.permissions.libraryAccessMode === "NONE"
              ? "无媒体库"
              : `指定 ${preset.permissions.libraryIds.length} 个媒体库`;
          return (
            <article className="lux-admin-panel" key={preset.id}>
              <div className="lux-admin-panel-heading">
                <div><h3>{preset.name}</h3><p className="lux-admin-muted">{preset.isBuiltin ? "内置预设" : `自定义预设 · v${preset.version}`}</p></div>
                <div className="lux-admin-user-profile-actions">
                  <LuxButton tone="secondary" type="button" aria-label={`编辑预设 ${preset.name}`} onClick={() => startEdit(preset)} disabled={busy || preset.isBuiltin}>编辑</LuxButton>
                  <LuxButton tone="secondary" type="button" aria-label={`删除预设 ${preset.name}`} onClick={() => deletePreset(preset)} disabled={busy || preset.isBuiltin}>删除</LuxButton>
                </div>
              </div>
              <p className="lux-admin-muted">{preset.description || "未填写说明"} · {preset.isAdmin ? "管理员" : "普通用户"}{preset.canManageServer ? "，可进入管理台" : ""}</p>
              <p className="lux-admin-muted">{librarySummary} · {preset.permissions.canDownload ? "允许下载" : "禁止下载"} · {preset.permissions.canPlay ? "允许播放" : "禁止播放"} · 并发 {preset.permissions.maxSimultaneousPlaybacks === 0 ? "不限" : preset.permissions.maxSimultaneousPlaybacks}</p>
              <p className="lux-admin-muted">封装转换 {preset.permissions.allowRemux ? "允许" : "禁止"} · 音频转码 {preset.permissions.allowAudioTranscode ? "允许" : "禁止"} · 视频转码 {preset.permissions.allowVideoTranscode ? "允许" : "禁止"}</p>
            </article>
          );
        })}
      </div>
      {draft ? (
        <form className="lux-admin-form lux-admin-user-defaults-form" onSubmit={save}>
          <div className="lux-admin-panel-heading"><div><h3>{editingPreset ? `编辑预设：${editingPreset.name}` : "新建角色预设"}</h3></div></div>
          <label>预设名称<LuxTextField aria-label="预设名称" maxLength={80} value={draft.name} disabled={busy} onChange={(event) => updateDraft({ name: event.currentTarget.value })} /></label>
          <label>说明<LuxTextField aria-label="预设说明" maxLength={500} value={draft.description ?? ""} disabled={busy} onChange={(event) => updateDraft({ description: event.currentTarget.value || null })} /></label>
          <fieldset className="lux-admin-user-defaults-policy-options">
            <legend>账号角色</legend>
            <LuxCheckbox aria-label="管理员角色" checked={draft.isAdmin} disabled={busy} onChange={(event) => updateDraft({ isAdmin: event.currentTarget.checked })} label="管理员角色" />
            <LuxCheckbox aria-label="允许进入管理台" checked={draft.canManageServer} disabled={busy || !draft.isAdmin} onChange={(event) => updateDraft({ canManageServer: event.currentTarget.checked })} label="允许进入管理台" />
          </fieldset>
          <fieldset className="lux-admin-user-access-modes">
            <legend>媒体库访问范围</legend>
            {([
              ["ALL", "全部媒体库"],
              ["SELECTED", "指定媒体库"],
              ["NONE", "无媒体库权限"],
            ] as const).map(([mode, title]) => (
              <LuxRadio key={mode} name="preset-library-access" value={mode} checked={draft.permissions.libraryAccessMode === mode} disabled={busy} onChange={() => updatePermissionDraft({ libraryAccessMode: mode, libraryIds: mode === "SELECTED" ? draft.permissions.libraryIds : [] })} label=<><strong>{title}</strong></> />
            ))}
          </fieldset>
          {draft.permissions.libraryAccessMode === "SELECTED" ? (
            <>
              {enabledLibraries.length ? (
                <div className="lux-admin-library-access">
                  {enabledLibraries.map((library) => (
                    <LuxCheckbox key={library.id} checked={selectedLibraryIds.includes(library.id)} disabled={busy} onChange={(event) => toggleLibrary(library.id, event.currentTarget.checked)} label=<>{library.name}</> />
                  ))}
                </div>
              ) : <p className="lux-admin-muted">当前没有已启用的媒体库；空选择保存后会设为无媒体库权限。</p>}
              {enabledLibraries.length > 0 && selectedLibraryIds.length === 0 ? <p className="lux-admin-muted">空选择保存后会设为无媒体库权限。</p> : null}
            </>
          ) : null}
          <fieldset className="lux-admin-user-defaults-policy-options">
            <legend>下载与播放/转码</legend>
            <LuxCheckbox aria-label="允许下载" checked={draft.permissions.canDownload} disabled={busy} onChange={(event) => updatePermissionDraft({ canDownload: event.currentTarget.checked })} label="允许下载" />
            <LuxCheckbox aria-label="允许播放" checked={draft.permissions.canPlay} disabled={busy} onChange={(event) => updatePermissionDraft({ canPlay: event.currentTarget.checked })} label="允许播放" />
            <LuxCheckbox aria-label="允许封装转换" checked={draft.permissions.allowRemux} disabled={busy} onChange={(event) => updatePermissionDraft({ allowRemux: event.currentTarget.checked })} label="允许封装转换" />
            <LuxCheckbox aria-label="允许音频转码" checked={draft.permissions.allowAudioTranscode} disabled={busy} onChange={(event) => updatePermissionDraft({ allowAudioTranscode: event.currentTarget.checked })} label="允许音频转码" />
            <LuxCheckbox aria-label="允许视频转码" checked={draft.permissions.allowVideoTranscode} disabled={busy} onChange={(event) => updatePermissionDraft({ allowVideoTranscode: event.currentTarget.checked })} label="允许视频转码" />
          </fieldset>
          <label className="lux-admin-user-playback-limit">同时播放上限（0 表示不限）<LuxTextField type="number" aria-label="预设同时播放上限" min={0} step={1} value={limitDraft} disabled={busy} onChange={(event) => { setLimitDraft(event.currentTarget.value); setError(""); setNotice(""); }} /></label>
          {!validName ? <p className="lux-admin-user-playback-validation">请输入 1–80 个字符的预设名称。</p> : null}
          {!validDescription ? <p className="lux-admin-user-playback-validation">说明不能超过 500 个字符。</p> : null}
          {!validLimit ? <p className="lux-admin-user-playback-validation">请输入大于或等于 0 的整数。</p> : null}
          <p className="lux-admin-muted">应用预设时会复制权限快照；以后修改预设不会自动改变已有用户。</p>
          <div className="lux-admin-user-profile-actions">
            <LuxButton tone="primary" type="submit" disabled={!canSave || busy}>{busy ? "保存中…" : "保存预设"}</LuxButton>
            <LuxButton tone="secondary" type="button" disabled={busy} onClick={() => { setDraft(null); setEditingPreset(null); setError(""); }}>取消</LuxButton>
            {error ? <span role="alert">{error}</span> : notice ? <span role="status">{notice}</span> : null}
          </div>
        </form>
      ) : notice ? <p role="status">{notice}</p> : error ? <p role="alert">{error}</p> : null}
    </section>
  );
}
