import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Image, RefreshCw, ScanSearch, Trash2 } from "lucide-react";
import { useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminMetadataCandidate } from "../../lib/api/types";

export function AdminMetadataPage() {
  const queryClient = useQueryClient();
  const pending = useQuery({ queryKey: queryKeys.adminPendingMetadata, queryFn: () => api.adminPendingMetadata() });
  if (pending.isPending) return <AdminMetadataState label="正在读取待处理元数据…" />;
  if (pending.error) return <AdminMetadataState label={pending.error.message} error />;
  const items = pending.data.items ?? [];
  return <div className="lux-admin-page"><header className="lux-admin-page-heading"><div><h1>元数据纠错</h1><p>查看低置信度匹配，确认候选并写回 NFO 与图片。</p></div><button className="lux-button lux-button-secondary" type="button" onClick={() => void pending.refetch()}><RefreshCw size={16} /> 刷新</button></header><section className="lux-admin-panel"><div className="lux-admin-panel-heading"><div><h2>待处理候选</h2></div><span className="lux-status-pill">{pending.data.total ?? items.length} 个</span></div>{items.length === 0 ? <div className="lux-admin-empty"><Check size={25} /><h2>没有待处理匹配</h2><p>当前所有条目的元数据都已确认。</p></div> : <div className="lux-admin-metadata-list">{items.map((item) => <MetadataCandidateRow key={item.id} item={item} onSelected={() => { void queryClient.invalidateQueries({ queryKey: queryKeys.adminPendingMetadata }); }} />)}</div>}</section></div>;
}

function MetadataCandidateRow({ item, onSelected }: { item: AdminMetadataCandidate; onSelected: () => void }) {
  const [showImages, setShowImages] = useState(false);
  const candidateTitle = typeof item.candidate.title === "string" ? item.candidate.title : item.providerId;
  const select = useMutation({ mutationFn: (mode: "fillMissing" | "refreshUnlocked") => api.selectAdminMetadata(item.itemId, item.id, mode), onSuccess: onSelected });
  const images = useQuery({ queryKey: queryKeys.adminItemImages(item.itemId), queryFn: () => api.adminItemImages(item.itemId), enabled: showImages });
  const deleteImage = useMutation({ mutationFn: (imageId: string) => api.deleteAdminItemImage(item.itemId, imageId), onSuccess: () => void images.refetch() });
  return <article className="lux-admin-metadata-card"><div className="lux-admin-metadata-heading"><div><span className="lux-eyebrow">{item.provider} · {Math.round(item.score)} 分</span><h3>{item.itemTitle}</h3><p>候选：{candidateTitle}</p></div><span className="lux-job-status status-pending">{item.status}</span></div>{item.fieldDiffs.length > 0 ? <div className="lux-admin-diff-list">{item.fieldDiffs.map((diff) => <div key={diff.field}><span>{diff.field}</span><small>{stringify(diff.current) || "空"} → {stringify(diff.candidate) || "空"}</small></div>)}</div> : <p className="lux-admin-muted">没有字段差异。</p>}<div className="lux-admin-metadata-actions"><button className="lux-button lux-button-primary" type="button" disabled={select.isPending} onClick={() => select.mutate("fillMissing")}><Check size={15} /> 仅补缺</button><button className="lux-button lux-button-secondary" type="button" disabled={select.isPending} onClick={() => select.mutate("refreshUnlocked")}><ScanSearch size={15} /> 刷新未锁定字段</button><button className="lux-button lux-button-secondary" type="button" onClick={() => setShowImages((value) => !value)}><Image size={15} /> {showImages ? "收起图片" : "查看图片"}</button></div>{select.error ? <p className="lux-error-copy">{select.error.message}</p> : null}{showImages ? <div className="lux-admin-image-list">{images.isPending ? <span className="lux-admin-muted">读取图片记录…</span> : (images.data?.images ?? []).length === 0 ? <span className="lux-admin-muted">暂无图片记录。</span> : (images.data?.images ?? []).map((image) => <div key={image.id}><span>{image.imageType} #{image.imageIndex}</span><small>{image.source || "未知来源"} · {formatBytes(image.fileSize)}</small><button className="lux-icon-button lux-icon-button-small lux-danger-icon" type="button" aria-label={`删除 ${image.imageType} 图片`} onClick={() => deleteImage.mutate(image.id)} disabled={deleteImage.isPending}><Trash2 size={14} /></button></div>)}</div> : null}</article>;
}

function stringify(value: unknown) { return typeof value === "string" ? value : value == null ? "" : JSON.stringify(value); }
function formatBytes(value?: number | null) { if (!value) return "未知大小"; return `${Math.max(1, Math.round(value / 1024))} KB`; }
function AdminMetadataState({ label, error = false }: { label: string; error?: boolean }) { return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{error ? "元数据加载失败" : "正在加载元数据"}</h1><p>{label}</p></section>; }
