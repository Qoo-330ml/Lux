import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useState } from "react";
import { api } from "../../../../lib/api/client";
import type { AdminUser, AdminUserPlaybackPolicy } from "../../../../lib/api/types";

/**
 * 「播放与转码」表单状态：策略草稿、并发上限输入、脏值判断与保存动作。
 * 同时播放上限用字符串保存，便于保留用户输入的空值与非数字中间态。
 */
export function usePlaybackPolicyForm(user: AdminUser) {
  const queryClient = useQueryClient();
  const queryKey = ["admin", "user", user.id, "playback-policy"];
  const policyQuery = useQuery({
    queryKey,
    queryFn: () => api.adminUserPlaybackPolicy(user.id),
  });
  const [draft, setDraft] = useState<AdminUserPlaybackPolicy | null>(null);
  const [limitDraft, setLimitDraft] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const save = useMutation({
    mutationFn: (policy: AdminUserPlaybackPolicy) => api.updateAdminUserPlaybackPolicy(user.id, policy),
    onSuccess: ({ policy }) => {
      setDraft(policy);
      setLimitDraft(String(policy.maxSimultaneousPlaybacks));
      setError("");
      setNotice("播放策略已保存。");
      queryClient.setQueryData(queryKey, { policy });
    },
    onError: (cause) => {
      setNotice("");
      setError(cause.message || "播放策略保存失败，请重试。");
    },
  });
  const currentPolicy = policyQuery.data?.policy;
  const currentDraft = draft ?? currentPolicy;
  const limitInput = limitDraft ?? (currentPolicy ? String(currentPolicy.maxSimultaneousPlaybacks) : "");
  const parsedLimit = Number(limitInput);
  const validLimit = limitInput.trim() !== "" && Number.isSafeInteger(parsedLimit) && parsedLimit >= 0;
  const updatedDraft = currentDraft && validLimit ? { ...currentDraft, maxSimultaneousPlaybacks: parsedLimit } : currentDraft;
  const hasChanges = updatedDraft != null && currentPolicy != null && (
    updatedDraft.canPlay !== currentPolicy.canPlay
    || updatedDraft.maxSimultaneousPlaybacks !== currentPolicy.maxSimultaneousPlaybacks
    || updatedDraft.allowRemux !== currentPolicy.allowRemux
    || updatedDraft.allowAudioTranscode !== currentPolicy.allowAudioTranscode
    || updatedDraft.allowVideoTranscode !== currentPolicy.allowVideoTranscode
  );

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!updatedDraft || !validLimit) return;
    setNotice("");
    setError("");
    save.mutate(updatedDraft);
  }

  return {
    policyQuery,
    currentDraft,
    setDraft,
    setLimitDraft,
    limitInput,
    validLimit,
    hasChanges,
    notice,
    error,
    save,
    submit,
  };
}
