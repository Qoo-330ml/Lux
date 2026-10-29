import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useState } from "react";
import { ApiError, api } from "../../../../lib/api/client";
import type {
  AdminUser,
  AdminUserContentRestrictionPolicy,
  AdminUserContentRestrictionUpdateInput,
} from "../../../../lib/api/types";

export const contentRatingLevels = {
  MPA: ["G", "PG", "PG-13", "R", "NC-17"],
  TVPG: ["TV-Y", "TV-G", "TV-Y7", "TV-PG", "TV-14", "TV-MA"],
  BBFC: ["U", "PG", "12", "12A", "15", "18", "R18"],
} as const;

type ContentRestrictionFormDraft = Pick<
  AdminUserContentRestrictionPolicy,
  "ratingSystem" | "maximumRating" | "allowUnrated"
> & {
  allowedTagsText: string;
  blockedTagsText: string;
};

/**
 * 用户内容限制表单的读取、编辑与保存状态：版本冲突时需要重新读取策略，
 * 因此把草稿、提示和保存动作放在一起，组件只负责渲染。
 */
export function useContentRestrictionForm(user: AdminUser) {
  const queryClient = useQueryClient();
  const queryKey = ["admin", "user", user.id, "content-restrictions"];
  const policyQuery = useQuery({
    queryKey,
    queryFn: () => api.adminUserContentRestrictions(user.id),
    enabled: user.deletedAt == null,
  });
  const [draft, setDraft] = useState<ContentRestrictionFormDraft | null>(null);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const [versionConflict, setVersionConflict] = useState(false);
  const currentPolicy = policyQuery.data?.policy ?? null;
  const baselineDraft = contentRestrictionFormDraft(currentPolicy);
  const currentDraft = draft ?? baselineDraft;
  const hasChanges = !sameContentRestrictionDraft(currentDraft, baselineDraft);
  const save = useMutation({
    mutationFn: (input: AdminUserContentRestrictionUpdateInput) =>
      api.updateAdminUserContentRestrictions(user.id, input),
    onSuccess: ({ policy }) => {
      setDraft(contentRestrictionFormDraft(policy));
      setError("");
      setVersionConflict(false);
      setNotice("内容限制已保存。");
      queryClient.setQueryData(queryKey, { policy });
    },
    onError: (cause) => {
      setNotice("");
      if (cause instanceof ApiError && cause.status === 409) {
        setVersionConflict(true);
        setError("策略已被其他管理员更改。请重新读取最新策略后再保存。");
        return;
      }
      setError(cause.message || "内容限制保存失败，请重试。");
    },
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!hasChanges || versionConflict) return;
    setError("");
    setNotice("");
    save.mutate({
      expectedVersion: currentPolicy?.version ?? 0,
      ratingSystem: currentDraft.ratingSystem,
      maximumRating: currentDraft.maximumRating,
      allowUnrated: currentDraft.allowUnrated,
      allowedTags: parseContentRestrictionTags(currentDraft.allowedTagsText),
      blockedTags: parseContentRestrictionTags(currentDraft.blockedTagsText),
    });
  }

  function reloadLatestPolicy() {
    setError("");
    setNotice("");
    void policyQuery.refetch().then((result) => {
      if (result.error || !result.data) {
        setError(`重新读取策略失败：${result.error?.message || "没有收到策略数据"}`);
        return;
      }
      setDraft(contentRestrictionFormDraft(result.data.policy));
      setVersionConflict(false);
      setNotice("已载入最新策略，请检查后再保存。");
    });
  }

  return {
    policyQuery,
    currentPolicy,
    currentDraft,
    setDraft,
    hasChanges,
    notice,
    error,
    versionConflict,
    save,
    submit,
    reloadLatestPolicy,
  };
}

function contentRestrictionFormDraft(policy: AdminUserContentRestrictionPolicy | null): ContentRestrictionFormDraft {
  return {
    ratingSystem: policy?.ratingSystem ?? "MPA",
    maximumRating: policy?.maximumRating ?? null,
    allowUnrated: policy?.allowUnrated ?? true,
    allowedTagsText: policy?.allowedTags.join("\n") ?? "",
    blockedTagsText: policy?.blockedTags.join("\n") ?? "",
  };
}

function sameContentRestrictionDraft(left: ContentRestrictionFormDraft, right: ContentRestrictionFormDraft) {
  return left.ratingSystem === right.ratingSystem
    && left.maximumRating === right.maximumRating
    && left.allowUnrated === right.allowUnrated
    && left.allowedTagsText === right.allowedTagsText
    && left.blockedTagsText === right.blockedTagsText;
}

function parseContentRestrictionTags(value: string) {
  return value.split(/\r?\n/).map((tag) => tag.trim()).filter(Boolean);
}
