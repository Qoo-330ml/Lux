import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Play } from "lucide-react";
import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { GLOBAL_COLLECTION_LIBRARY_ID, type CollectionMember, type CollectionSourceCopy, type CollectionSourceVersion } from "../../lib/api/types";
import { mediaTitle, posterUrlWithFallback } from "../home/media";
import { useImageVariants } from "../home/image-variants";

const COLLECTION_PAGE_SIZE = 24;

type CollectionSourceChoice = {
  value: string;
  source: CollectionSourceCopy;
  version?: CollectionSourceVersion;
};

type SourcePreferenceChange = {
  workItemId: string;
  sourceItemId: string;
  mediaSourceId?: string;
  value: string;
};

export function CollectionDetailPage() {
  const { collectionId = "" } = useParams();
  const { enabled } = useImageVariants();
  const queryClient = useQueryClient();
  const [selectedChoices, setSelectedChoices] = useState<Record<string, string>>({});
  const collectionQuery = useInfiniteQuery({
    queryKey: queryKeys.collection(collectionId),
    queryFn: ({ pageParam }) => api.collection(collectionId, pageParam, COLLECTION_PAGE_SIZE),
    initialPageParam: 1,
    enabled: Boolean(collectionId),
    getNextPageParam: (lastPage) => {
      const page = lastPage.page ?? 1;
      const pageSize = lastPage.pageSize ?? COLLECTION_PAGE_SIZE;
      return page * pageSize < lastPage.total ? page + 1 : undefined;
    },
  });
  const preference = useMutation({
    mutationFn: (change: SourcePreferenceChange) => api.setCollectionSourcePreference(
      collectionId,
      change.workItemId,
      { sourceItemId: change.sourceItemId, mediaSourceId: change.mediaSourceId },
    ),
    onSuccess: (_result, change) => {
      setSelectedChoices((current) => ({ ...current, [change.workItemId]: change.value }));
      void queryClient.invalidateQueries({ queryKey: queryKeys.collection(collectionId) });
    },
  });

  if (collectionQuery.isPending) {
    return <section className="lux-page-state" aria-busy="true"><p>正在加载合集影片…</p></section>;
  }
  if (collectionQuery.error) {
    return <section className="lux-page-state" role="alert"><h1>合集加载失败</h1><p>{collectionQuery.error.message}</p><Link to={`/libraries/${GLOBAL_COLLECTION_LIBRARY_ID}`}>返回合集库</Link></section>;
  }

  const firstPage = collectionQuery.data.pages[0];
  if (!firstPage) return null;
  const collection = firstPage.collection;
  const members = collectionQuery.data.pages.flatMap((page) => page.items ?? []);
  const poster = posterUrlWithFallback(collection, undefined, enabled ? 720 : undefined);

  return (
    <main className="lux-page lux-collection-detail">
      <Link className="lux-button lux-button-secondary lux-collection-back" to={`/libraries/${GLOBAL_COLLECTION_LIBRARY_ID}`}>
        <ArrowLeft size={16} aria-hidden="true" /> 返回合集库
      </Link>
      <header className="lux-collection-heading">
        {poster ? <img className="lux-collection-poster" src={poster} alt={`${mediaTitle(collection)} 海报`} /> : <div className="lux-collection-poster lux-collection-poster-placeholder" aria-hidden="true">合集</div>}
        <div>
          <span className="lux-eyebrow">系列合集</span>
          <h1>{mediaTitle(collection)}</h1>
          <p>{collection.productionYear ? `${collection.productionYear} · ` : ""}{firstPage.total} 部影片 · 按影片上映顺序排列</p>
          {collection.overview ? <p className="lux-collection-overview">{collection.overview}</p> : null}
        </div>
      </header>

      {members.length ? (
        <ol className="lux-collection-work-list" aria-label={`${mediaTitle(collection)} 影片列表`}>
          {members.map((member) => (
            <CollectionWorkRow
              key={member.id}
              collectionId={collectionId}
              member={member}
              selectedValue={selectedChoices[member.id]}
              isSaving={preference.isPending}
              saveError={preference.variables?.workItemId === member.id ? preference.error?.message : undefined}
              onSelect={(change) => preference.mutate(change)}
            />
          ))}
        </ol>
      ) : (
        <div className="lux-empty-card" role="status">
          <span>这个系列目前没有可见影片。</span>
          <Link to={`/libraries/${GLOBAL_COLLECTION_LIBRARY_ID}`}>返回合集库</Link>
        </div>
      )}

      {collectionQuery.hasNextPage ? (
        <button className="lux-button lux-button-secondary lux-collection-load-more" type="button" disabled={collectionQuery.isFetchingNextPage} onClick={() => void collectionQuery.fetchNextPage()}>
          {collectionQuery.isFetchingNextPage ? "正在加载…" : "加载更多影片"}
        </button>
      ) : null}
      {collectionQuery.isFetchNextPageError ? <p className="lux-error-copy" role="alert">加载更多影片失败，请稍后重试。</p> : null}
    </main>
  );
}

function CollectionWorkRow({
  collectionId,
  member,
  selectedValue,
  isSaving,
  saveError,
  onSelect,
}: {
  collectionId: string;
  member: CollectionMember;
  selectedValue?: string;
  isSaving: boolean;
  saveError?: string;
  onSelect: (change: SourcePreferenceChange) => void;
}) {
  const choices = sourceChoices(member.sources);
  const preferred = choices.find((choice) =>
    choice.source.itemId === member.preferredSourceItemId
    && (!member.preferredMediaSourceId || choice.version?.id === member.preferredMediaSourceId),
  );
  const currentValue = selectedValue ?? preferred?.value ?? choices[0]?.value ?? "";
  const selected = choices.find((choice) => choice.value === currentValue);
  const poster = selected?.source.posterImageTag
    ? `/api/v1/items/${encodeURIComponent(selected.source.itemId)}/images/poster?tag=${encodeURIComponent(selected.source.posterImageTag)}`
    : undefined;
  const detailQuery = new URLSearchParams({ collectionId });
  if (selected?.version) detailQuery.set("sourceId", selected.version.id);
  const detailHref = selected
    ? `/items/${encodeURIComponent(selected.source.itemId)}?${detailQuery.toString()}`
    : undefined;

  function handleSourceChange(value: string) {
    const choice = choices.find((entry) => entry.value === value);
    if (!choice) return;
    onSelect({
      workItemId: member.id,
      sourceItemId: choice.source.itemId,
      ...(choice.version ? { mediaSourceId: choice.version.id } : {}),
      value,
    });
  }

  return (
    <li className="lux-collection-work">
      <div className="lux-collection-work-art">
        {poster ? <img src={poster} alt="" loading="lazy" decoding="async" /> : <span aria-hidden="true">{mediaTitle(member).slice(0, 1)}</span>}
      </div>
      <div className="lux-collection-work-copy">
        <h2>{mediaTitle(member)}</h2>
        <p>{member.productionYear ?? "年份未知"}{member.runtimeTicks ? ` · ${Math.max(1, Math.round(member.runtimeTicks / 600_000_000))} 分钟` : ""}</p>
        {choices.length ? (
          <label className="lux-collection-source-control">
            <span>来源与版本</span>
            <select aria-label={`选择《${mediaTitle(member)}》的可访问来源`} value={currentValue} disabled={isSaving} onChange={(event) => handleSourceChange(event.target.value)}>
              {choices.map((choice) => <option key={choice.value} value={choice.value}>{sourceChoiceLabel(choice.source, choice.version)}</option>)}
            </select>
          </label>
        ) : <p role="status">该影片目前没有可访问的播放副本。</p>}
        {saveError ? <p className="lux-error-copy" role="alert">来源保存失败：{saveError}</p> : null}
        {detailHref ? <Link className="lux-button lux-button-primary lux-collection-open-movie" data-action="open-selected-movie" to={detailHref}><Play size={15} fill="currentColor" aria-hidden="true" /> 打开影片详情</Link> : null}
      </div>
    </li>
  );
}

function sourceChoices(sources: CollectionSourceCopy[]): CollectionSourceChoice[] {
  return sources.flatMap((source) => {
    const versions = source.versions.length ? source.versions : [undefined];
    return versions.map((version) => ({
      value: JSON.stringify([source.itemId, version?.id ?? null]),
      source,
      version,
    }));
  });
}

function sourceChoiceLabel(source: CollectionSourceCopy, version?: CollectionSourceVersion) {
  const versionLabel = [version?.editionName, version?.qualityLabel, version?.container?.toUpperCase()]
    .filter((value): value is string => Boolean(value?.trim()))
    .join(" · ");
  return [source.libraryName, source.title, versionLabel].filter(Boolean).join(" — ");
}
