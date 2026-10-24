import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminItemMergeRecord, MediaItem } from "../../lib/api/types";

const MERGEABLE_ITEM_TYPES = new Set(["MOVIE", "SERIES"]);

function originLabel(origin: string) {
  return origin === "AUTO" ? "自动合并" : "手动合并";
}

function mergedAtLabel(seconds: number) {
  const date = new Date(seconds * 1000);
  if (!Number.isFinite(date.getTime())) return "";
  return date.toLocaleString("zh-CN", { hour12: false });
}

export function MediaMergeSourcesSection({ item }: { item: MediaItem }) {
  const queryClient = useQueryClient();
  const session = useQuery({
    queryKey: queryKeys.me,
    queryFn: () => api.me(),
    retry: false,
    staleTime: 5 * 60 * 1000,
  });
  const canManage = session.data?.user?.canManageServer === true
    && MERGEABLE_ITEM_TYPES.has(item.itemType ?? "");
  const records = useQuery({
    queryKey: queryKeys.adminItemMerges(item.id),
    queryFn: () => api.adminItemMerges(item.id),
    enabled: canManage,
  });
  const [confirmingId, setConfirmingId] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const unmerge = useMutation({
    mutationFn: (record: AdminItemMergeRecord) =>
      api.unmergeAdminItems({ primaryItemId: item.id, mergedItemId: record.mergedItemId }),
    onSuccess: async (result, record) => {
      setConfirmingId(undefined);
      setNotice(`已拆分「${record.mergedItemTitle}」，恢复 ${result.restoredSourceCount} 个媒体源。`);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.adminItemMerges(item.id) }),
        queryClient.invalidateQueries({ queryKey: queryKeys.item(item.id) }),
        queryClient.invalidateQueries({ queryKey: queryKeys.libraries }),
        queryClient.invalidateQueries({ queryKey: queryKeys.home }),
      ]);
    },
  });

  if (!canManage) return null;
  const activeRecords = (records.data?.records ?? []).filter((record) => record.undoneAt == null);
  if (records.isPending) {
    return (
      <section className="lux-detail-merge-sources" aria-label="合并来源">
        <p className="lux-muted-copy">正在读取合并来源…</p>
      </section>
    );
  }
  if (records.error) {
    return (
      <section className="lux-detail-merge-sources" aria-label="合并来源">
        <p className="lux-error-copy" role="alert">合并来源读取失败：{records.error.message}</p>
      </section>
    );
  }
  if (activeRecords.length === 0 && !notice && !unmerge.error) return null;

  return (
    <section className="lux-detail-merge-sources" aria-label="合并来源">
      <h2 className="lux-detail-section-title">合并来源</h2>
      {activeRecords.length > 0 ? (
        <ul className="lux-detail-merge-list">
          {activeRecords.map((record) => (
            <li key={record.id} className="lux-detail-merge-item">
              <span className="lux-detail-merge-title">{record.mergedItemTitle}</span>
              <span className="lux-detail-merge-meta">
                {originLabel(record.origin)} · {record.sourceCount} 个媒体源
                {record.mergedAt ? ` · ${mergedAtLabel(record.mergedAt)}` : ""}
              </span>
              {confirmingId === record.id ? (
                <span className="lux-detail-merge-confirm">
                  <span className="lux-muted-copy">拆分后恢复为独立条目，播放进度保留在此条目。</span>
                  <button
                    className="lux-button lux-button-secondary"
                    type="button"
                    disabled={unmerge.isPending}
                    onClick={() => setConfirmingId(undefined)}
                  >
                    取消
                  </button>
                  <button
                    className="lux-button lux-button-primary"
                    type="button"
                    data-action="confirm-unmerge"
                    disabled={unmerge.isPending}
                    onClick={() => unmerge.mutate(record)}
                  >
                    {unmerge.isPending ? "拆分中…" : "确认拆分"}
                  </button>
                </span>
              ) : (
                <button
                  className="lux-button lux-button-secondary"
                  type="button"
                  data-action="unmerge-item"
                  aria-label={`拆分已合并条目 ${record.mergedItemTitle}`}
                  disabled={unmerge.isPending}
                  onClick={() => {
                    unmerge.reset();
                    setNotice(undefined);
                    setConfirmingId(record.id);
                  }}
                >
                  拆分
                </button>
              )}
            </li>
          ))}
        </ul>
      ) : null}
      {notice ? <p className="lux-muted-copy" role="status">{notice}</p> : null}
      {unmerge.error ? <p className="lux-error-copy" role="alert">拆分失败：{unmerge.error.message}</p> : null}
    </section>
  );
}
