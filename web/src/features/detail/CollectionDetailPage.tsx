import "./detail.css";
import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft } from "lucide-react";
import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { GLOBAL_COLLECTION_LIBRARY_ID, type CollectionMember, type CollectionSourceCopy, type CollectionSourceVersion } from "../../lib/api/types";
import { HorizontalScrollRail } from "../../components/layout/HorizontalScrollRail";
import { mediaTitle, MediaPlaceholder } from "../home/media-display";
import { MediaCardCopy, MediaCardVisual } from "../home/media-card-visual";
import { MediaDetailArtwork } from "./MediaDetailHero";

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
  const title = mediaTitle(collection);
  const originalTitle = collection.originalTitle?.trim();

  return (
    <main className="lux-detail-page lux-collection-detail">
      <MediaDetailArtwork media={collection} placement="backdrop" />
      <div className="lux-detail-overlay" />
      <div className="lux-detail-content">
        <Link className="lux-detail-collection-return lux-collection-back" to={`/libraries/${GLOBAL_COLLECTION_LIBRARY_ID}`}>
          <ArrowLeft size={16} aria-hidden="true" /> 返回合集库
        </Link>
        <header className="lux-detail-grid">
          <MediaDetailArtwork media={collection} placement="poster" />
          <div className="lux-detail-copy">
            <div className="lux-detail-title-row">
              <div className="lux-detail-title-names">
                <h1>{title}</h1>
                {originalTitle && originalTitle !== title ? <span className="lux-detail-original-title">{originalTitle}</span> : null}
              </div>
            </div>
            <div className="lux-detail-meta">
              <span>系列合集</span>
              {collection.productionYear ? <span>{collection.productionYear}</span> : null}
              <span>{firstPage.total} 部影片</span>
            </div>
            <p className="lux-detail-overview lux-collection-overview">{collection.overview?.trim() || "暂无简介。"}</p>
          </div>
        </header>

        <section className="lux-section lux-collection-members" aria-labelledby="collection-members-heading">
          <div className="lux-section-heading">
            <h2 id="collection-members-heading">合集影片</h2>
            <span>按影片上映顺序排列</span>
          </div>
          {members.length ? (
            <HorizontalScrollRail className="lux-home-rail" ariaLabel="合集影片">
              <ol className="lux-media-rail lux-collection-work-list" aria-label={`${title} 影片列表`}>
                {members.map((member) => (
                  <CollectionWorkCard
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
            </HorizontalScrollRail>
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
        </section>
      </div>
    </main>
  );
}

function CollectionWorkCard({
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
  const sourceItem = selected ? {
    ...member,
    id: selected.source.itemId,
    libraryId: selected.source.libraryId,
    posterItemId: undefined,
    imageTags: { poster: selected.source.posterImageTag ?? undefined },
  } : undefined;
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
    <li className="lux-media-card lux-collection-work">
      {sourceItem && detailHref ? <>
        <MediaCardVisual item={sourceItem} landscape={false} href={detailHref} />
        <MediaCardCopy item={sourceItem} metadataAttention={false} href={detailHref} />
      </> : <>
        <div className="lux-media-art"><MediaPlaceholder item={member} /></div>
        <div className="lux-media-copy"><strong>{mediaTitle(member)}</strong></div>
        <p className="lux-muted-copy" role="status">该影片目前没有可访问的播放副本。</p>
      </>}
      {choices.length > 1 ? (
        <label className="lux-collection-source-control">
          <span>来源与版本</span>
          <select aria-label={`选择《${mediaTitle(member)}》的可访问来源`} value={currentValue} disabled={isSaving} onChange={(event) => handleSourceChange(event.target.value)}>
            {choices.map((choice) => <option key={choice.value} value={choice.value}>{sourceChoiceLabel(choice.source, choice.version)}</option>)}
          </select>
        </label>
      ) : null}
      {saveError ? <p className="lux-error-copy" role="alert">来源保存失败：{saveError}</p> : null}
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
