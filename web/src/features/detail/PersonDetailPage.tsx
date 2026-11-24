import "./detail.css";
import { ArrowLeft, Heart, UserRound, X } from "lucide-react";
import { Fragment, type FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useInfiniteQuery, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { LuxUser, MediaItem, PersonDetail } from "../../lib/api/types";
import { MediaCard, mediaTypeLabel } from "../home/media";
import { LuxButton, LuxTextField, LuxTextarea } from "../../components/ui";

type PersonDetailPageProps = { user?: LuxUser };

type PersonDraft = {
  name: string;
  biography: string;
  birthday: string;
  deathday: string;
  knownForDepartment: string;
  placeOfBirth: string;
  providerIds: string;
  genres: string;
  tags: string;
  productionLocations: string;
  premiereDate: string;
  productionYear: string;
  taglines: string;
};

type YearGroup = {
  key: string;
  label: string;
  items: MediaItem[];
};

const BIO_CLAMP_THRESHOLD = 110;

export function PersonDetailPage({ user }: PersonDetailPageProps) {
  const { personId = "" } = useParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const person = useQuery({
    queryKey: queryKeys.person(personId),
    queryFn: () => api.person(personId),
    enabled: Boolean(personId),
  });
  const works = useInfiniteQuery({
    queryKey: queryKeys.personItems(personId),
    queryFn: ({ pageParam }) => api.personItems(personId, pageParam),
    initialPageParam: 1,
    enabled: Boolean(personId && person.data),
    getNextPageParam: (lastPage) => {
      const page = lastPage.page ?? 1;
      const pageSize = lastPage.pageSize ?? 24;
      const total = lastPage.total ?? 0;
      return page * pageSize < total ? page + 1 : undefined;
    },
  });
  const [imageFailed, setImageFailed] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState<PersonDraft>();
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string>();
  const [favoriteSaving, setFavoriteSaving] = useState(false);
  const [favoriteError, setFavoriteError] = useState<string>();
  const [bioExpanded, setBioExpanded] = useState(false);
  const [worksFilter, setWorksFilter] = useState<string>("all");
  const dossierDialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    setImageFailed(false);
    setBioExpanded(false);
    setWorksFilter("all");
  }, [personId]);

  useEffect(() => {
    const dialog = dossierDialogRef.current;
    if (!dialog || "closedBy" in HTMLDialogElement.prototype) return;
    let pressedOutside = false;
    const outside = (event: PointerEvent) => {
      const rect = dialog.getBoundingClientRect();
      return (
        event.clientX < rect.left
        || event.clientX > rect.right
        || event.clientY < rect.top
        || event.clientY > rect.bottom
      );
    };
    const onDown = (event: PointerEvent) => {
      pressedOutside = outside(event);
    };
    const onUp = (event: PointerEvent) => {
      if (pressedOutside && outside(event)) dialog.close();
    };
    dialog.addEventListener("pointerdown", onDown);
    dialog.addEventListener("pointerup", onUp);
    return () => {
      dialog.removeEventListener("pointerdown", onDown);
      dialog.removeEventListener("pointerup", onUp);
    };
  }, [person.data]);

  const workItems = useMemo(
    () => works.data?.pages.flatMap((page) => page.items ?? []) ?? [],
    [works.data],
  );
  const totalWorksCount = works.data?.pages[0]?.total ?? workItems.length;
  const worksSummary = useMemo(
    () => computeWorksSummary(workItems, totalWorksCount),
    [workItems, totalWorksCount],
  );
  const typeCounts = useMemo(() => collectTypeCounts(workItems), [workItems]);
  const activeFilter = worksFilter === "all" || typeCounts.some(([type]) => type === worksFilter)
    ? worksFilter
    : "all";
  const filteredWorks = useMemo(
    () => (activeFilter === "all" ? workItems : workItems.filter((item) => item.itemType === activeFilter)),
    [activeFilter, workItems],
  );
  const yearGroups = useMemo(() => groupWorksByYear(filteredWorks), [filteredWorks]);

  if (person.isPending) {
    return <section className="lux-page-state" aria-busy="true"><p>正在加载人物详情…</p></section>;
  }
  if (person.error) {
    return <section className="lux-page-state" role="alert"><h1>人物详情加载失败</h1><p>{person.error.message}</p><LuxButton tone="secondary" type="button" onClick={() => navigate(-1)}>返回</LuxButton></section>;
  }

  const detail = person.data;
  const image = detail.imageUrl && !imageFailed ? (
    <img key={detail.id} className="lux-person-detail-photo" src={detail.imageUrl} alt={`${detail.name} 照片`} onError={() => setImageFailed(true)} />
  ) : (
    <div className="lux-person-detail-placeholder" role="img" aria-label={`${detail.name} 暂无照片`}><UserRound size={42} strokeWidth={1.4} /></div>
  );
  const providerIds = Object.entries(detail.providerIds ?? {});
  const genres = detail.genres ?? [];
  const tags = detail.tags ?? [];
  const locations = detail.productionLocations ?? [];
  const taglines = detail.taglines ?? [];
  const canEdit = Boolean(user?.canManageServer);
  const biographyText = buildBiographyText(detail);
  const clampableBio = shouldClampBiography(biographyText);
  const hasSecondaryDossier = Boolean(
    detail.birthday
    || detail.deathday
    || detail.knownForDepartment
    || detail.placeOfBirth
    || detail.premiereDate
    || detail.productionYear
    || locations.length
    || providerIds.length
    || genres.length
    || tags.length
    || taglines.length,
  );

  const summaryItems = [
    worksSummary.yearSpan,
    totalWorksCount > 0 ? `${totalWorksCount} 部入库作品` : undefined,
    detail.birthday && detail.placeOfBirth
      ? `${detail.birthday} · ${detail.placeOfBirth}`
      : detail.birthday || detail.placeOfBirth,
    detail.knownForDepartment,
    detail.character ? `饰演：${detail.character}` : undefined,
  ].filter((item): item is string => Boolean(item));

  function startEditing() {
    setDraft(draftFromPerson(detail));
    setSaveError(undefined);
    setEditing(true);
  }

  function cancelEditing() {
    setEditing(false);
    setDraft(undefined);
    setSaveError(undefined);
  }

  async function toggleFavorite() {
    const favorite = !Boolean(detail.isFavorite);
    setFavoriteSaving(true);
    setFavoriteError(undefined);
    try {
      await api.setPersonFavorite(personId, favorite);
      queryClient.setQueryData<PersonDetail>(queryKeys.person(personId), (current) =>
        current ? { ...current, isFavorite: favorite } : current,
      );
    } catch (cause) {
      setFavoriteError(cause instanceof Error ? cause.message : "演员收藏状态保存失败，请重试。");
    } finally {
      setFavoriteSaving(false);
    }
  }

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!draft) return;
    const productionYear = draft.productionYear.trim();
    if (productionYear && (!/^\d+$/.test(productionYear) || !Number.isSafeInteger(Number(productionYear)))) {
      setSaveError("年份必须是有效整数。");
      return;
    }
    setSaving(true);
    setSaveError(undefined);
    try {
      const updated = await api.updatePerson(personId, {
        name: draft.name.trim(),
        biography: draft.biography,
        birthday: draft.birthday,
        deathday: draft.deathday,
        knownForDepartment: draft.knownForDepartment,
        placeOfBirth: draft.placeOfBirth,
        providerIds: parseProviderIds(draft.providerIds),
        genres: parseLines(draft.genres),
        tags: parseLines(draft.tags),
        productionLocations: parseLines(draft.productionLocations),
        premiereDate: draft.premiereDate,
        productionYear: productionYear ? Number(productionYear) : undefined,
        taglines: parseLines(draft.taglines),
      });
      queryClient.setQueryData(queryKeys.person(personId), updated);
      setEditing(false);
      setDraft(undefined);
    } catch (cause) {
      setSaveError(cause instanceof Error ? cause.message : "人物资料保存失败，请重试。");
    } finally {
      setSaving(false);
    }
  }

  return (
    <article className="lux-person-detail-page">
      <button className="lux-person-back" type="button" onClick={() => navigate(-1)}><ArrowLeft size={17} /> 返回</button>

      <header className="lux-person-hero">
        <div className="lux-person-detail-photo-frame">{image}</div>

        <div className="lux-person-detail-copy">
          <div className="lux-person-title-row">
            {editing && draft ? (
              <LuxTextField
                id="person-name"
                className="lux-person-title-input"
                value={draft.name}
                onChange={(event) => setDraft({ ...draft, name: event.target.value })}
                aria-label="人物姓名"
              />
            ) : (
              <h1>{detail.name}</h1>
            )}

            <div className="lux-person-hero-actions">
              <button
                type="button"
                className="lux-person-fav-circle"
                aria-label={detail.isFavorite ? "取消收藏演员" : "收藏演员"}
                aria-pressed={Boolean(detail.isFavorite)}
                title={detail.isFavorite ? "取消收藏演员" : "收藏演员"}
                onClick={() => void toggleFavorite()}
                disabled={favoriteSaving}
              >
                <Heart size={18} fill={detail.isFavorite ? "currentColor" : "none"} />
              </button>
              {!editing && hasSecondaryDossier ? (
                <LuxButton
                  tone="secondary"
                  size="compact"
                  type="button"
                  data-open-dossier=""
                  onClick={() => dossierDialogRef.current?.showModal()}
                >
                  完整档案
                </LuxButton>
              ) : null}
              {canEdit && !editing ? (
                <LuxButton
                  tone="secondary"
                  size="compact"
                  className="lux-person-edit-button"
                  type="button"
                  aria-label="编辑人物资料"
                  onClick={startEditing}
                >
                  编辑资料
                </LuxButton>
              ) : null}
              {canEdit && editing ? (
                <div className="lux-person-edit-actions">
                  <LuxButton tone="primary" size="compact" type="submit" form="person-editor" aria-label="保存人物资料" disabled={saving}>
                    {saving ? "保存中…" : "保存"}
                  </LuxButton>
                  <LuxButton tone="secondary" size="compact" type="button" onClick={cancelEditing} disabled={saving}>
                    取消
                  </LuxButton>
                </div>
              ) : null}
            </div>
          </div>

          {favoriteError ? <p className="lux-error-copy" role="alert">{favoriteError}</p> : null}

          {!editing ? (
            <>
              {summaryItems.length > 0 ? (
                <p className="lux-person-summary">
                  {summaryItems.map((entry, index) => (
                    <span key={`${entry}-${index}`}>{entry}</span>
                  ))}
                </p>
              ) : null}
              <section className="lux-person-overview" aria-label="演员简介">
                <RichText text={biographyText} clamped={clampableBio && !bioExpanded} />
                {clampableBio || hasSecondaryDossier ? (
                  <div className="lux-person-bio-actions">
                    {clampableBio ? (
                      <button
                        type="button"
                        className="lux-person-bio-toggle"
                        aria-expanded={bioExpanded}
                        onClick={() => setBioExpanded((current) => !current)}
                      >
                        {bioExpanded ? "收起简介" : "展开全部简介"}
                      </button>
                    ) : null}
                    {hasSecondaryDossier ? (
                      <button
                        type="button"
                        className="lux-person-bio-toggle"
                        onClick={() => dossierDialogRef.current?.showModal()}
                      >
                        外部 ID 与更多资料
                      </button>
                    ) : null}
                  </div>
                ) : null}
              </section>
            </>
          ) : null}
        </div>
      </header>

      {editing && draft ? (
        <section className="lux-person-editor-panel" aria-label="编辑人物资料">
          <PersonEditor draft={draft} setDraft={setDraft} onSubmit={save} error={saveError} />
        </section>
      ) : null}

      <section className="lux-person-works" aria-labelledby="person-works-heading">
        <div className="lux-person-works-toolbar">
          <div className="lux-section-heading">
            <h2 id="person-works-heading">参演作品</h2>
            <span>{totalWorksCount} 项</span>
          </div>
          {typeCounts.length > 1 ? (
            <div className="lux-person-filter-chips" role="group" aria-label="按类型筛选作品">
              <button
                type="button"
                className="lux-person-filter-chip"
                data-works-filter="all"
                aria-pressed={activeFilter === "all"}
                onClick={() => setWorksFilter("all")}
              >
                全部作品 ({workItems.length})
              </button>
              {typeCounts.map(([type, count]) => (
                <button
                  key={type}
                  type="button"
                  className="lux-person-filter-chip"
                  data-works-filter={type}
                  aria-pressed={activeFilter === type}
                  onClick={() => setWorksFilter(type)}
                >
                  {mediaTypeLabel(type)} ({count})
                </button>
              ))}
            </div>
          ) : null}
        </div>

        {works.isPending ? <p className="lux-muted-copy">正在加载参演作品…</p> : null}
        {works.error ? <p className="lux-error-copy" role="alert">参演作品加载失败：{works.error.message}</p> : null}
        {!works.isPending && !works.error && !workItems.length ? <p className="lux-muted-copy">暂无已入库的参演电影或剧集。</p> : null}

        {yearGroups.length > 0 ? (
          <div className="lux-person-timeline">
            {yearGroups.map((group) => (
              <section key={group.key} className="lux-person-timeline-group" aria-label={`${group.label}作品`}>
                <span className="lux-person-timeline-node" aria-hidden="true" />
                <div className="lux-person-timeline-header">
                  <h3 className="lux-person-timeline-year-title">{group.label}</h3>
                  <span className="lux-person-timeline-count">{group.items.length} 部</span>
                </div>
                <div className="lux-poster-grid lux-person-works-grid">
                  {group.items.map((item) => <MediaCard item={item} key={item.id} />)}
                </div>
              </section>
            ))}
          </div>
        ) : null}

        {works.hasNextPage ? (
          <div className="lux-person-works-actions">
            <LuxButton tone="secondary" type="button" onClick={() => void works.fetchNextPage()} disabled={works.isFetchingNextPage}>
              {works.isFetchingNextPage ? "加载中…" : "加载更多"}
            </LuxButton>
          </div>
        ) : null}
      </section>

      <dialog
        ref={dossierDialogRef}
        className="lux-person-dossier-dialog"
        {...({ closedby: "any" } as Record<string, string>)}
        aria-labelledby="person-dossier-title"
      >
        <div className="lux-person-dossier-head">
          <h2 id="person-dossier-title">{detail.name} · 完整档案</h2>
          <form method="dialog">
            <button type="submit" className="lux-person-dossier-close" aria-label="关闭弹窗" autoFocus>
              <X size={17} />
            </button>
          </form>
        </div>
        <div className="lux-person-dossier-body">
          <dl className="lux-person-facts">
            {detail.birthday ? <div><dt>出生日期</dt><dd>{detail.birthday}</dd></div> : null}
            {detail.deathday ? <div><dt>去世日期</dt><dd>{detail.deathday}</dd></div> : null}
            {detail.knownForDepartment ? <div><dt>职业领域</dt><dd>{detail.knownForDepartment}</dd></div> : null}
            {detail.placeOfBirth ? <div><dt>出生地</dt><dd>{detail.placeOfBirth}</dd></div> : null}
            {detail.premiereDate ? <div><dt>首次记录日期</dt><dd>{detail.premiereDate}</dd></div> : null}
            {detail.productionYear ? <div><dt>年份</dt><dd>{detail.productionYear}</dd></div> : null}
            {locations.length > 0 ? <div><dt>地区</dt><dd>{locations.join("、")}</dd></div> : null}
          </dl>
          {providerIds.length > 0 ? (
            <section className="lux-person-metadata-group" aria-labelledby="person-provider-heading">
              <h3 id="person-provider-heading">资料来源</h3>
              <dl className="lux-person-metadata-list">
                {providerIds.map(([provider, id]) => <div key={`${provider}-${id}`}><dt>{provider}</dt><dd>{id}</dd></div>)}
              </dl>
            </section>
          ) : null}
          {genres.length > 0 ? <MetadataGroup heading="类型" values={genres} /> : null}
          {tags.length > 0 ? <MetadataGroup heading="标签" values={tags} /> : null}
          {taglines.length > 0 ? <MetadataGroup heading="标语" values={taglines} /> : null}
          <section className="lux-person-dossier-bio" aria-label="完整简介">
            <h3>简介</h3>
            <RichText text={biographyText} />
          </section>
        </div>
      </dialog>
    </article>
  );
}

function resolveWorkYear(item: MediaItem): number | undefined {
  if (item.productionYear != null && Number.isInteger(item.productionYear) && item.productionYear >= 1800 && item.productionYear <= 2999) {
    return item.productionYear;
  }
  const match = item.premiereDate?.match(/^(\d{4})\b/);
  if (!match) return undefined;
  const year = Number(match[1]);
  return Number.isInteger(year) && year >= 1800 && year <= 2999 ? year : undefined;
}

function groupWorksByYear(items: MediaItem[]): YearGroup[] {
  const byYear = new Map<number, MediaItem[]>();
  const unknown: MediaItem[] = [];
  for (const item of items) {
    const year = resolveWorkYear(item);
    if (year == null) {
      unknown.push(item);
      continue;
    }
    const bucket = byYear.get(year);
    if (bucket) {
      bucket.push(item);
    } else {
      byYear.set(year, [item]);
    }
  }
  const groups: YearGroup[] = Array.from(byYear.entries())
    .sort(([left], [right]) => right - left)
    .map(([year, groupItems]) => ({
      key: String(year),
      label: `${year}年`,
      items: groupItems,
    }));
  if (unknown.length > 0) {
    groups.push({ key: "unknown", label: "年份未知", items: unknown });
  }
  return groups;
}

function computeWorksSummary(items: MediaItem[], total: number): { yearSpan?: string; total: number } {
  const years = items.map(resolveWorkYear).filter((year): year is number => year != null);
  if (years.length === 0) return { total };
  const minYear = Math.min(...years);
  const maxYear = Math.max(...years);
  return {
    yearSpan: minYear === maxYear ? `${minYear}年` : `${minYear}年–${maxYear}年`,
    total,
  };
}

function collectTypeCounts(items: MediaItem[]): Array<[string, number]> {
  const counts = new Map<string, number>();
  for (const item of items) {
    const key = item.itemType || "OTHER";
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return Array.from(counts.entries());
}

function buildBiographyText(detail: PersonDetail): string {
  if (detail.biography?.trim()) return detail.biography;
  const parts: string[] = [];
  if (detail.birthday && detail.placeOfBirth) {
    parts.push(`${detail.birthday}出生于${detail.placeOfBirth}`);
  } else if (detail.birthday) {
    parts.push(`生于 ${detail.birthday}`);
  } else if (detail.placeOfBirth) {
    parts.push(`出生于${detail.placeOfBirth}`);
  }
  if (detail.knownForDepartment) {
    parts.push(`主要从事 ${detail.knownForDepartment} 工作`);
  }
  return parts.length > 0 ? `${detail.name}，${parts.join("，")}。` : "暂无简介。";
}

function shouldClampBiography(text: string): boolean {
  if (text.length > BIO_CLAMP_THRESHOLD) return true;
  return /<br\s*\/?>|\n/i.test(text);
}

function PersonEditor({
  draft,
  setDraft,
  onSubmit,
  error,
}: {
  draft: PersonDraft;
  setDraft: (draft: PersonDraft) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  error?: string;
}) {
  const field = (key: keyof PersonDraft, label: string, multiline = false) => (
    <label className="lux-person-editor-field" htmlFor={`person-${key}`}>
      <span>{label}</span>
      {multiline ? <LuxTextarea id={`person-${key}`} value={draft[key]} rows={3} onChange={(event) => setDraft({ ...draft, [key]: event.target.value })} /> : <LuxTextField id={`person-${key}`} value={draft[key]} onChange={(event) => setDraft({ ...draft, [key]: event.target.value })} />}
    </label>
  );
  return (
    <form id="person-editor" className="lux-person-editor" onSubmit={onSubmit}>
      {field("biography", "简介", true)}
      <div className="lux-person-editor-grid">
        {field("birthday", "出生日期")}
        {field("deathday", "去世日期")}
        {field("knownForDepartment", "职业领域")}
        {field("placeOfBirth", "出生地")}
        {field("premiereDate", "首次记录日期")}
        {field("productionYear", "年份")}
      </div>
      <p className="lux-person-editor-help">列表字段每行填写一项；外部 ID 使用 provider=id 格式。</p>
      {field("providerIds", "外部 ID", true)}
      {field("genres", "类型", true)}
      {field("tags", "标签", true)}
      {field("productionLocations", "地区", true)}
      {field("taglines", "标语", true)}
      {error ? <p className="lux-error-copy" role="alert">{error}</p> : null}
    </form>
  );
}

function draftFromPerson(person: PersonDetail): PersonDraft {
  return {
    name: person.name,
    biography: person.biography ?? "",
    birthday: person.birthday ?? "",
    deathday: person.deathday ?? "",
    knownForDepartment: person.knownForDepartment ?? "",
    placeOfBirth: person.placeOfBirth ?? "",
    providerIds: Object.entries(person.providerIds ?? {}).map(([provider, id]) => `${provider}=${id}`).join("\n"),
    genres: (person.genres ?? []).join("\n"),
    tags: (person.tags ?? []).join("\n"),
    productionLocations: (person.productionLocations ?? []).join("\n"),
    premiereDate: person.premiereDate ?? "",
    productionYear: person.productionYear ? String(person.productionYear) : "",
    taglines: (person.taglines ?? []).join("\n"),
  };
}

function parseLines(value: string): string[] {
  return value.split(/\r?\n/).map((item) => item.trim()).filter(Boolean);
}

function parseProviderIds(value: string): Record<string, string> {
  return Object.fromEntries(parseLines(value).flatMap((line) => {
    const separator = line.indexOf("=");
    if (separator <= 0) return [];
    const provider = line.slice(0, separator).trim();
    const id = line.slice(separator + 1).trim();
    return provider && id ? [[provider, id]] : [];
  }));
}

function MetadataGroup({ heading, values }: { heading: string; values: string[] }) {
  return (
    <section className="lux-person-metadata-group" aria-label={heading}>
      <h3>{heading}</h3>
      <ul className="lux-person-metadata-values">
        {values.map((value, index) => <li key={`${value}-${index}`}>{value}</li>)}
      </ul>
    </section>
  );
}

function RichText({ text, clamped = false }: { text: string; clamped?: boolean }) {
  const normalized = text.replace(/&lt;br\s*\/?&gt;/gi, "<br>");
  const lines = normalized.split(/<br\s*\/?>/gi);
  return (
    <p className={clamped ? "is-clamped" : undefined}>
      {lines.map((line, index) => <Fragment key={`${line}-${index}`}>{index > 0 ? <br /> : null}{line}</Fragment>)}
    </p>
  );
}
