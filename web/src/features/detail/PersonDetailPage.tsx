import { ArrowLeft, Cake, Heart, MapPin, UserRound } from "lucide-react";
import { Fragment, type FormEvent, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { LuxUser, PersonDetail } from "../../lib/api/types";

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

export function PersonDetailPage({ user }: PersonDetailPageProps) {
  const { personId = "" } = useParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const person = useQuery({
    queryKey: queryKeys.person(personId),
    queryFn: () => api.person(personId),
    enabled: Boolean(personId),
  });
  const [imageFailed, setImageFailed] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState<PersonDraft>();
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string>();
  const [favoriteSaving, setFavoriteSaving] = useState(false);
  const [favoriteError, setFavoriteError] = useState<string>();

  if (person.isPending) {
    return <section className="lux-page-state" aria-busy="true"><p>正在加载人物详情…</p></section>;
  }
  if (person.error) {
    return <section className="lux-page-state" role="alert"><h1>人物详情加载失败</h1><p>{person.error.message}</p><button className="lux-button lux-button-secondary" type="button" onClick={() => navigate(-1)}>返回</button></section>;
  }

  const detail = person.data;
  const image = detail.imageUrl && !imageFailed ? (
    <img className="lux-person-detail-photo" src={detail.imageUrl} alt={`${detail.name} 照片`} onError={() => setImageFailed(true)} />
  ) : (
    <div className="lux-person-detail-placeholder" role="img" aria-label={`${detail.name} 暂无照片`}><UserRound size={58} strokeWidth={1.4} /></div>
  );
  const providerIds = Object.entries(detail.providerIds ?? {});
  const genres = detail.genres ?? [];
  const tags = detail.tags ?? [];
  const locations = detail.productionLocations ?? [];
  const taglines = detail.taglines ?? [];
  const canEdit = Boolean(user?.canManageServer);

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
      <div className="lux-person-detail-grid">
        <div className="lux-person-detail-photo-frame">{image}</div>
        <div className="lux-person-detail-copy">
          <p className="lux-eyebrow">人物资料</p>
          <div className="lux-person-title-row">
            {editing && draft ? <input id="person-name" className="lux-person-title-input" value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} aria-label="人物姓名" /> : <h1>{detail.name}</h1>}
            <button className="lux-button lux-button-secondary" type="button" aria-label={detail.isFavorite ? "取消收藏演员" : "收藏演员"} aria-pressed={Boolean(detail.isFavorite)} onClick={() => void toggleFavorite()} disabled={favoriteSaving}>
              <Heart size={16} fill={detail.isFavorite ? "currentColor" : "none"} /> {favoriteSaving ? "保存中…" : detail.isFavorite ? "已收藏" : "收藏演员"}
            </button>
            {canEdit && !editing ? <button className="lux-button lux-button-secondary lux-person-edit-button" type="button" aria-label="编辑人物资料" onClick={startEditing}>编辑资料</button> : null}
            {canEdit && editing ? <div className="lux-person-edit-actions"><button className="lux-button lux-button-primary" type="submit" form="person-editor" aria-label="保存人物资料" disabled={saving}>{saving ? "保存中…" : "保存"}</button><button className="lux-button lux-button-secondary" type="button" onClick={cancelEditing} disabled={saving}>取消</button></div> : null}
          </div>
          {favoriteError ? <p className="lux-error-copy" role="alert">{favoriteError}</p> : null}
          {detail.character ? <p className="lux-person-detail-role">饰演：{detail.character}</p> : null}
          {editing && draft ? <PersonEditor draft={draft} setDraft={setDraft} onSubmit={save} error={saveError} /> : <>
          <dl className="lux-person-facts">
            {detail.birthday ? <div><dt><Cake size={15} />出生日期</dt><dd>{detail.birthday}</dd></div> : null}
            {detail.deathday ? <div><dt><Cake size={15} />去世日期</dt><dd>{detail.deathday}</dd></div> : null}
            {detail.knownForDepartment ? <div><dt>职业领域</dt><dd>{detail.knownForDepartment}</dd></div> : null}
            {detail.placeOfBirth ? <div><dt><MapPin size={15} />出生地</dt><dd>{detail.placeOfBirth}</dd></div> : null}
            {detail.premiereDate ? <div><dt>首次记录日期</dt><dd>{detail.premiereDate}</dd></div> : null}
            {detail.productionYear ? <div><dt>年份</dt><dd>{detail.productionYear}</dd></div> : null}
            {locations.length > 0 ? <div><dt>地区</dt><dd>{locations.join("、")}</dd></div> : null}
          </dl>
          {providerIds.length > 0 ? (
            <section className="lux-person-metadata-group" aria-labelledby="person-provider-heading">
              <h2 id="person-provider-heading">资料来源</h2>
              <dl className="lux-person-metadata-list">
                {providerIds.map(([provider, id]) => <div key={`${provider}-${id}`}><dt>{provider}</dt><dd>{id}</dd></div>)}
              </dl>
            </section>
          ) : null}
          {genres.length > 0 ? <MetadataGroup heading="类型" values={genres} /> : null}
          {tags.length > 0 ? <MetadataGroup heading="标签" values={tags} /> : null}
          {taglines.length > 0 ? <MetadataGroup heading="标语" values={taglines} /> : null}
          <section className="lux-person-overview" aria-labelledby="person-overview-heading">
            <h2 id="person-overview-heading">简介</h2>
            <RichText text={detail.biography || "暂无简介。"} />
          </section>
          </>}
        </div>
      </div>
    </article>
  );
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
      {multiline ? <textarea id={`person-${key}`} value={draft[key]} rows={3} onChange={(event) => setDraft({ ...draft, [key]: event.target.value })} /> : <input id={`person-${key}`} value={draft[key]} onChange={(event) => setDraft({ ...draft, [key]: event.target.value })} />}
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
      <h2>{heading}</h2>
      <ul className="lux-person-metadata-values">
        {values.map((value, index) => <li key={`${value}-${index}`}>{value}</li>)}
      </ul>
    </section>
  );
}

function RichText({ text }: { text: string }) {
  const normalized = text.replace(/&lt;br\s*\/?&gt;/gi, "<br>");
  const lines = normalized.split(/<br\s*\/?>/gi);
  return (
    <p>
      {lines.map((line, index) => <Fragment key={`${line}-${index}`}>{index > 0 ? <br /> : null}{line}</Fragment>)}
    </p>
  );
}
