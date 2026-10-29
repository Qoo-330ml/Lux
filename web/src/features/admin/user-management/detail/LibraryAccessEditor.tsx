import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { api } from "../../../../lib/api/client";
import { queryKeys } from "../../../../lib/api/query-keys";
import type { AdminUser, AdminUserLibraryAccessMode } from "../../../../lib/api/types";
import { LuxButton, LuxCheckbox, LuxRadio } from "../../../../components/ui";
import { AdminUserManagementState } from "../AdminUserManagementState";

export function AdminUserLibraryAccessEditor({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const queryKey = ["admin", "user-access", user.id];
  const accessQuery = useQuery({
    queryKey,
    queryFn: () => api.adminUserLibraryAccess(user.id),
  });
  const librariesQuery = useQuery({
    queryKey: queryKeys.adminLibraries,
    queryFn: () => api.adminLibraries(),
  });
  const [modeDraft, setModeDraft] = useState<AdminUserLibraryAccessMode | null>(null);
  const [libraryDraft, setLibraryDraft] = useState<string[] | null>(null);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const availableLibraries = librariesQuery.data?.libraries ?? [];
  const enabledLibraries = availableLibraries.filter((library) => library.isEnabled);
  const enabledLibraryIds = new Set(enabledLibraries.map((library) => library.id));
  const storedMode = accessQuery.data?.libraryAccessMode;
  const draftMode = modeDraft ?? storedMode ?? "ALL";
  const storedLibraryIds = accessQuery.data?.libraryIds ?? [];
  const draftLibraryIds = libraryDraft ?? storedLibraryIds;
  const enabledDraftIds = draftLibraryIds.filter((id) => enabledLibraryIds.has(id));
  const disabledDraftNames = draftLibraryIds
    .filter((id) => !enabledLibraryIds.has(id))
    .map((id) => availableLibraries.find((library) => library.id === id)?.name ?? id);
  const saveMode: AdminUserLibraryAccessMode = draftMode === "SELECTED" && enabledDraftIds.length === 0
    ? "NONE"
    : draftMode;
  const saveLibraryIds = saveMode === "SELECTED" ? enabledDraftIds : [];
  const savedLibraryIds = storedMode === "SELECTED"
    ? storedLibraryIds.filter((id) => enabledLibraryIds.has(id))
    : [];
  const hasChanges = storedMode != null && (
    saveMode !== storedMode
    || saveLibraryIds.length !== savedLibraryIds.length
    || saveLibraryIds.some((id, index) => id !== savedLibraryIds[index])
  );
  const update = useMutation({
    mutationFn: () => api.updateAdminUserLibraryAccess(user.id, {
      libraryAccessMode: saveMode,
      libraryIds: saveLibraryIds,
    }),
    onSuccess: (result) => {
      setModeDraft(result.libraryAccessMode);
      setLibraryDraft(result.libraryIds);
      setError("");
      setNotice("媒体库访问权限已保存。");
      queryClient.setQueryData(queryKey, result);
    },
    onError: (cause) => {
      setNotice("");
      setError(cause.message || "媒体库权限保存失败，请重试。");
    },
  });
  useEffect(() => {
    if (!accessQuery.data) return;
    setModeDraft(accessQuery.data.libraryAccessMode);
    setLibraryDraft(accessQuery.data.libraryIds);
  }, [accessQuery.data, user.id]);

  if (accessQuery.isPending || librariesQuery.isPending) {
    return <AdminUserManagementState label="正在读取媒体库权限…" />;
  }
  if (accessQuery.error) {
    return <AdminUserManagementState label={`读取媒体库权限失败：${accessQuery.error.message}`} error />;
  }
  if (librariesQuery.error) {
    return <AdminUserManagementState label={`读取媒体库列表失败：${librariesQuery.error.message}`} error />;
  }
  if (!accessQuery.data) {
    return <AdminUserManagementState label="暂时无法读取媒体库权限。" error />;
  }

  function toggleLibrary(libraryId: string, checked: boolean) {
    setLibraryDraft((current) => {
      const currentIds = current ?? storedLibraryIds;
      const next = new Set(currentIds);
      if (checked) next.add(libraryId);
      else next.delete(libraryId);
      const enabledIds = enabledLibraries.map((library) => library.id).filter((id) => next.has(id));
      return [...enabledIds, ...currentIds.filter((id) => !enabledLibraryIds.has(id))];
    });
    setNotice("");
    setError("");
  }

  return (
    <section className="lux-admin-panel lux-admin-user-permissions" aria-labelledby="lux-admin-user-permissions-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-permissions-title">权限</h2></div></div>
      <form
        className="lux-admin-form lux-admin-user-access-form"
        onSubmit={(event) => {
          event.preventDefault();
          setNotice("");
          setError("");
          update.mutate();
        }}
      >
        <fieldset className="lux-admin-user-access-modes">
          <legend>媒体库访问范围</legend>
          <LuxRadio name={`library-access-${user.id}`} value="ALL" checked={draftMode === "ALL"} disabled={update.isPending} onChange={() => { setModeDraft("ALL"); setNotice(""); setError(""); }} label=<><strong>全部媒体库</strong><small>允许访问所有已启用媒体库，包括之后新增的媒体库。</small></> />
          <LuxRadio name={`library-access-${user.id}`} value="SELECTED" checked={draftMode === "SELECTED"} disabled={update.isPending} onChange={() => { setModeDraft("SELECTED"); setNotice(""); setError(""); }} label=<><strong>指定媒体库</strong><small>仅允许访问下方勾选的媒体库。</small></> />
          <LuxRadio name={`library-access-${user.id}`} value="NONE" checked={draftMode === "NONE"} disabled={update.isPending} onChange={() => { setModeDraft("NONE"); setNotice(""); setError(""); }} label=<><strong>无媒体库权限</strong><small>用户不能浏览、搜索或播放任何媒体库内容。</small></> />
        </fieldset>
        {draftMode === "ALL" ? <p className="lux-admin-muted">可访问所有已启用媒体库，包括之后新增的媒体库。</p> : null}
        {draftMode === "NONE" ? <p className="lux-admin-muted">不能访问任何媒体库。</p> : null}
        {draftMode === "SELECTED" ? (
          <>
            {disabledDraftNames.length ? (
              <p className="lux-admin-muted">已停用媒体库当前不可访问；保存指定范围时会移除：{disabledDraftNames.join("、")}。</p>
            ) : null}
            <p className="lux-admin-muted">
              {enabledLibraries.length
                ? enabledDraftIds.length
                  ? `已选择 ${enabledDraftIds.length} 个媒体库。`
                  : "请至少选择一个媒体库；空选择保存后会设为无媒体库权限。"
                : "当前没有已启用的媒体库；保存空选择后会设为无媒体库权限。"}
            </p>
            {enabledLibraries.length ? (
              <div className="lux-admin-library-access">
                {enabledLibraries.map((library) => (
                  <LuxCheckbox key={library.id} checked={enabledDraftIds.includes(library.id)} disabled={update.isPending} onChange={(event) => toggleLibrary(library.id, event.currentTarget.checked)} label=<>{library.name}</> />
                ))}
              </div>
            ) : null}
          </>
        ) : null}
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="primary" type="submit" disabled={!hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存媒体库权限"}
          </LuxButton>
          {error ? <span role="alert">保存失败：{error}</span> : null}
          {notice ? <span role="status">{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}
