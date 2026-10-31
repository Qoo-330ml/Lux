import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { api } from "../../../../lib/api/client";
import { queryKeys } from "../../../../lib/api/query-keys";
import type { AdminUser, AdminUserLibraryAccessMode } from "../../../../lib/api/types";

/**
 * 「权限 · 媒体库访问范围」表单状态：草稿、脏值判断、保存与勾选顺序维护。
 * 已停用的媒体库不计入保存结果，但保留在草稿里以便提示。
 */
export function useLibraryAccessForm(user: AdminUser) {
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

  return {
    accessQuery,
    librariesQuery,
    setModeDraft,
    setNotice,
    setError,
    notice,
    error,
    draftMode,
    enabledLibraries,
    enabledDraftIds,
    disabledDraftNames,
    hasChanges,
    update,
    toggleLibrary,
  };
}
