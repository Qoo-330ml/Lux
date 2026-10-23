import "./detail.css";
import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, ChevronRight, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
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

export function CollectionDetailPage() {
  const { collectionId = "" } = useParams();
  const [choosingMemberId, setChoosingMemberId] = useState<string>();
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
  const choosingMember = members.find((member) => member.id === choosingMemberId);

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
                    onChooseSource={() => setChoosingMemberId(member.id)}
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
      {choosingMember ? <CollectionSourceDialog collectionId={collectionId} member={choosingMember} onClose={() => setChoosingMemberId(undefined)} /> : null}
    </main>
  );
}

function CollectionWorkCard({
  collectionId,
  member,
  onChooseSource,
}: {
  collectionId: string;
  member: CollectionMember;
  onChooseSource: () => void;
}) {
  const choices = sourceChoices(member.sources);
  const preferred = choices.find((choice) =>
    choice.source.itemId === member.preferredSourceItemId
    && (!member.preferredMediaSourceId || choice.version?.id === member.preferredMediaSourceId),
  );
  const selected = preferred ?? choices[0];
  const sourceItem = selected ? {
    ...member,
    id: selected.source.itemId,
    libraryId: selected.source.libraryId,
    posterItemId: undefined,
    imageTags: { poster: selected.source.posterImageTag ?? undefined },
  } : undefined;
  const detailHref = selected ? collectionMovieHref(collectionId, selected) : undefined;
  const onActivate = member.sources.length > 1 ? onChooseSource : undefined;

  return (
    <li className="lux-media-card lux-collection-work">
      {sourceItem && detailHref ? <>
        <MediaCardVisual item={sourceItem} landscape={false} href={detailHref} onActivate={onActivate} />
        <MediaCardCopy item={sourceItem} metadataAttention={false} href={detailHref} onActivate={onActivate} />
      </> : <>
        <div className="lux-media-art"><MediaPlaceholder item={member} /></div>
        <div className="lux-media-copy"><strong>{mediaTitle(member)}</strong></div>
        <p className="lux-muted-copy" role="status">该影片目前没有可访问的播放副本。</p>
      </>}
    </li>
  );
}

function CollectionSourceDialog({ collectionId, member, onClose }: { collectionId: string; member: CollectionMember; onClose: () => void }) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const activeRef = useRef(false);
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const preference = useMutation({
    mutationFn: (choice: CollectionSourceChoice) => api.setCollectionSourcePreference(collectionId, member.id, {
      sourceItemId: choice.source.itemId,
      ...(choice.version ? { mediaSourceId: choice.version.id } : {}),
    }),
    onSuccess: (_result, choice) => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.collection(collectionId) });
      if (activeRef.current) navigate(collectionMovieHref(collectionId, choice));
    },
  });

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    activeRef.current = true;
    dialog.showModal();
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      activeRef.current = false;
      dialog.close();
      document.body.style.overflow = previousOverflow;
    };
  }, []);

  return (
    <dialog ref={dialogRef} className="lux-collection-source-dialog" aria-labelledby="collection-source-heading" aria-describedby="collection-source-description" onCancel={onClose} onClick={(event) => {
      if (event.target !== event.currentTarget) return;
      const bounds = event.currentTarget.getBoundingClientRect();
      if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) onClose();
    }}>
      <header className="lux-detail-overview-dialog-header">
        <div><h2 id="collection-source-heading">选择影片来源</h2><p id="collection-source-description">{mediaTitle(member)}</p></div>
        <button className="lux-detail-overview-dialog-close" type="button" aria-label="关闭来源选择" onClick={onClose} autoFocus><X size={19} aria-hidden="true" /></button>
      </header>
      <div className="lux-collection-source-body" aria-busy={preference.isPending}>
        <ul className="lux-collection-source-options">
          {sourceChoices(member.sources).map((choice) => (
            <li key={choice.value}>
              <button className="lux-collection-source-option" type="button" disabled={preference.isPending} onClick={() => preference.mutate(choice)}>
                <span><strong>{choice.source.libraryName}</strong><small>{sourceChoiceLabel(choice.source, choice.version)}</small></span>
                <ChevronRight size={18} aria-hidden="true" />
              </button>
            </li>
          ))}
        </ul>
        {preference.isPending ? <p className="lux-muted-copy" role="status">正在打开影片…</p> : null}
        {preference.error ? <p className="lux-error-copy" role="alert">来源保存失败：{preference.error.message}，请选择来源重试。</p> : null}
      </div>
    </dialog>
  );
}

function collectionMovieHref(collectionId: string, choice: CollectionSourceChoice) {
  const query = new URLSearchParams({ collectionId });
  if (choice.version) query.set("sourceId", choice.version.id);
  return `/items/${encodeURIComponent(choice.source.itemId)}?${query.toString()}`;
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
  return [source.title, versionLabel].filter(Boolean).join(" — ");
}
