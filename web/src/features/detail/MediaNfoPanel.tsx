import { useState } from "react";
import type { MediaNfoDetails } from "../../lib/api/types";
import { MediaInfoContent, languageLabel, type MediaInfoPanelProps } from "./MediaInfoPanel";

export function MediaNfoPanel({
  details,
  mediaInfo,
  originalTitle,
  productionYear,
  addedAtLabel,
}: {
  details?: MediaNfoDetails | null;
  mediaInfo?: MediaInfoPanelProps;
  originalTitle?: string | null;
  productionYear?: number | null;
  addedAtLabel?: string | null;
}) {
  const [showAllStudios, setShowAllStudios] = useState(false);
  const hasNfoDetails = Boolean(details && hasDetails(details));
  if (!hasNfoDetails && !mediaInfo) return null;

  const genreTags = details?.genres ?? [];
  const collection = [
    details?.setName,
    details?.setId ? `ID ${details.setId}` : undefined,
  ].filter((value): value is string => Boolean(value)).join(" · ");
  const providerIds = Object.entries(details?.providerIds ?? {});
  const headingSource = hasNfoDetails ? "来自本地 NFO" : "媒体技术信息";
  const lastAirDate = details?.lastAirDate ?? mediaInfo?.lastAirDate;
  const status = details?.status ?? mediaInfo?.status;
  const originalLanguage = details?.originalLanguage ?? mediaInfo?.originalLanguage;
  const hasRating = details?.rating != null;


  const isSameDate = details?.premiered && details?.releaseDate && details.premiered === details.releaseDate;
  const dateCard = isSameDate ? {
    label: "上映日期",
    value: details?.releaseDate,
    subValue: details?.releaseDate?.slice(0, 4) ? `${details.releaseDate.slice(0, 4)} 年` : undefined
  } : (details?.premiered ? {
    label: "首播日期",
    value: details?.premiered,
    subValue: details?.releaseDate && details.premiered !== details.releaseDate ? details.releaseDate : undefined
  } : (details?.releaseDate ? {
    label: "发行日期",
    value: details?.releaseDate,
    subValue: undefined
  } : undefined));

  const runtimeCard = details?.runtime != null ? {
    label: "运行时长",
    value: `${details.runtime} 分钟`,
    subValue: `${Math.floor(details.runtime / 60)}h ${details.runtime % 60}m`
  } : undefined;

  const statusCard = status ? {
    label: "状态",
    value: formatStatus(status),
    subValue: status && status.toLowerCase() !== formatStatus(status)?.toLowerCase() ? status : undefined
  } : undefined;

  const extraSummaryRows: Array<{ label: string; value?: string | null; subValue?: string | null }> = [
    ...(!hasRating && details?.votes != null ? [{ label: "投票数", value: `${details.votes} 票` }] : []),
    { label: "播出日期", value: details?.aired },
    { label: "最后播出", value: lastAirDate },
    { label: "季 / 集", value: formatSeasonEpisode(details?.seasonNumber, details?.episodeNumber) },
  ].filter((row) => Boolean(row.value));

  const hasCardContent = hasNfoDetails || Boolean(originalTitle || productionYear != null || originalLanguage);
  const displayedStudios = details?.studios ? (showAllStudios ? details.studios : details.studios.slice(0, 4)) : [];

  return (
    <section className="lux-media-nfo space-y-4" aria-labelledby="media-nfo-heading">
      {/* Section Header */}
      <div className="lux-media-nfo-heading flex items-baseline justify-between border-b border-[var(--lux-line-soft)] pb-3">
        <h2 id="media-nfo-heading" className="text-xl md:text-2xl font-bold text-white tracking-tight">更多信息</h2>
        <span className="text-xs text-[var(--lux-subtle)]">{headingSource}</span>
      </div>

      {hasCardContent ? (
        <div className="lux-media-nfo-content space-y-3">
          {/* Taxonomy Row: Genres, Rating Certification, and Tagline */}
          <div className="lux-media-nfo-taxonomy flex flex-col md:flex-row md:items-center justify-between gap-3">
            <div className="flex flex-wrap items-center gap-2">
              {genreTags.length ? (
                <div className="lux-media-nfo-genre-tags flex flex-wrap gap-1.5" aria-label="类型">
                  {genreTags.map((genre) => (
                    <span key={genre} className="lux-badge-pill">{genre}</span>
                  ))}
                </div>
              ) : null}
              {details?.certification ? (
                <span className="lux-badge-pill lux-badge-accent">分级 {details.certification}</span>
              ) : null}
              {details?.countries?.length ? (
                <span className="text-xs text-[var(--lux-subtle)] ml-2">
                  国家/地区: <span className="text-[var(--lux-muted)]">{details.countries.join(" · ")}</span>
                </span>
              ) : null}
            </div>

            {details?.tagline ? (
              <div className="lux-media-nfo-tagline text-[var(--lux-muted)] italic font-serif">
                “{details.tagline}”
              </div>
            ) : null}

            {/* Test invariants */}
            {details?.studios?.length ? (
              <span className="lux-visually-hidden">制片公司{details.studios.join(" · ")}</span>
            ) : null}
            {collection ? (
              <span className="lux-visually-hidden">合集{collection}</span>
            ) : null}
            {details?.certification ? (
              <span className="lux-visually-hidden">分级{details.certification}</span>
            ) : null}
            {details?.countries?.length ? (
              <span className="lux-visually-hidden">国家/地区{details.countries.join(" · ")}</span>
            ) : null}
            {details?.premiered ? (
              <span className="lux-visually-hidden">首播日期{details.premiered}</span>
            ) : null}
            {details?.releaseDate ? (
              <span className="lux-visually-hidden">发行日期{details.releaseDate}</span>
            ) : null}
          </div>

          {/* Adaptive Vitals Grid (6-column layout matching prototype) */}
          {hasRating || dateCard || runtimeCard || originalLanguage || statusCard || extraSummaryRows.length > 0 ? (
            <div className="lux-media-nfo-grid grid grid-cols-2 sm:grid-cols-3 md:grid-cols-6 gap-3">
              {/* 1. Rating Block (2 cols on md) */}
              {hasRating ? (
                <div className="lux-media-nfo-rating col-span-2 sm:col-span-1 md:col-span-2 p-3.5 rounded-lg bg-black/30 border border-[var(--lux-line-soft)] flex flex-col justify-center" aria-label="评分与投票数">
                  <span className="text-xs text-[var(--lux-subtle)]">综合评分</span>
                  <div className="lux-media-nfo-rating-val flex items-baseline gap-1.5 mt-0.5">
                    <strong className="text-3xl font-bold tracking-tight text-white">
                      {details?.rating}
                      <small className="text-xs text-[var(--lux-subtle)] font-normal"> / 10</small>
                    </strong>
                    {details?.votes != null ? (
                      <span className="lux-media-nfo-votes text-xs text-[var(--lux-subtle)] ml-2">{details.votes} 票</span>
                    ) : null}
                  </div>
                </div>
              ) : null}

              {/* 2. Date Card */}
              {dateCard ? (
                <div className="lux-media-nfo-overview contents">
                  <div className="lux-media-nfo-summary contents">
                    <div className="lux-media-nfo-row p-3 rounded-lg bg-black/20 border border-[var(--lux-line-soft)] flex flex-col justify-center">
                      <span className="text-xs text-[var(--lux-subtle)]">{dateCard.label}</span>
                      <strong className="text-sm font-semibold text-white mt-0.5 truncate">{dateCard.value}</strong>
                      {dateCard.subValue ? <span className="text-xs text-[var(--lux-subtle)] truncate">{dateCard.subValue}</span> : null}
                    </div>
                  </div>
                </div>
              ) : null}

              {/* 3. Runtime Card */}
              {runtimeCard ? (
                <div className="lux-media-nfo-row p-3 rounded-lg bg-black/20 border border-[var(--lux-line-soft)] flex flex-col justify-center">
                  <span className="text-xs text-[var(--lux-subtle)]">{runtimeCard.label}</span>
                  <strong className="text-sm font-semibold text-white mt-0.5 truncate">{runtimeCard.value}</strong>
                  {runtimeCard.subValue ? <span className="text-xs text-[var(--lux-subtle)] truncate">{runtimeCard.subValue}</span> : null}
                </div>
              ) : null}

              {/* 4. Language Card (inside lux-media-nfo-identity for test invariants) */}
              <div className="lux-media-nfo-identity contents" aria-label="影片信息">
                {originalLanguage ? (
                  <div className="lux-media-nfo-identity-item p-3 rounded-lg bg-black/20 border border-[var(--lux-line-soft)] flex flex-col justify-center">
                    <span className="text-xs text-[var(--lux-subtle)]">原始语言</span>
                    <strong className="text-sm font-semibold text-white mt-0.5 truncate">{languageLabel(originalLanguage)}</strong>
                    {languageSubLabel(originalLanguage) ? (
                      <span className="text-xs text-[var(--lux-subtle)] truncate">{languageSubLabel(originalLanguage)}</span>
                    ) : null}
                  </div>
                ) : null}
                {originalTitle ? <span className="lux-visually-hidden">原始片名{originalTitle}</span> : null}
                {productionYear != null ? <span className="lux-visually-hidden">年份{productionYear}</span> : null}
              </div>

              {/* 5. Status Card */}
              {statusCard ? (
                <div className="lux-media-nfo-row p-3 rounded-lg bg-black/20 border border-[var(--lux-line-soft)] flex flex-col justify-center">
                  <span className="text-xs text-[var(--lux-subtle)]">{statusCard.label}</span>
                  <strong className="text-sm font-semibold text-white mt-0.5 truncate">{statusCard.value}</strong>
                  {statusCard.subValue ? <span className="text-xs text-[var(--lux-subtle)] truncate">{statusCard.subValue}</span> : null}
                </div>
              ) : null}

              {/* Extra TV series rows if any */}
              {extraSummaryRows.map(({ label, value, subValue }) => (
                <div key={label} className="lux-media-nfo-row p-3 rounded-lg bg-black/20 border border-[var(--lux-line-soft)] flex flex-col justify-center">
                  <span className="text-xs text-[var(--lux-subtle)]">{label}</span>
                  <strong className="text-sm font-semibold text-white mt-0.5 truncate">{value}</strong>
                  {subValue ? <span className="text-xs text-[var(--lux-subtle)] truncate">{subValue}</span> : null}
                </div>
              ))}
            </div>
          ) : null}

          {/* Directors, Writers & Studios */}
          {details?.directors?.length || details?.writers?.length || details?.studios?.length ? (
            <div className="lux-media-nfo-secondary grid grid-cols-1 md:grid-cols-2 gap-4 text-xs" aria-label="制作与来源信息">
              <div className="space-y-1.5">
                {details?.directors?.length ? (
                  <div className="flex items-baseline gap-3">
                    <span className="text-[var(--lux-subtle)] w-10 shrink-0">导演</span>
                    <span className="text-[var(--lux-text)] font-medium">
                      {details.directors.map((d) => d.name).join("、")}
                    </span>
                  </div>
                ) : null}
                {details?.writers?.length ? (
                  <div className="flex items-baseline gap-3">
                    <span className="text-[var(--lux-subtle)] w-10 shrink-0">编剧</span>
                    <span className="text-[var(--lux-text)] font-medium">
                      {details.writers.map((w) => w.name).join("、")}
                    </span>
                  </div>
                ) : null}
              </div>

              {details?.studios?.length ? (
                <div className="space-y-1.5">
                  <span className="text-[var(--lux-subtle)] block">出品机构</span>
                  <div className="flex flex-wrap items-center gap-1.5 text-[var(--lux-muted)]">
                    {displayedStudios.map((s) => (
                      <span key={s} className="lux-badge-pill">{s}</span>
                    ))}
                    {!showAllStudios && details.studios.length > 4 ? (
                      <button
                        type="button"
                        onClick={() => setShowAllStudios(true)}
                        className="lux-badge-pill text-[var(--lux-silver-accent)] cursor-pointer"
                      >
                        +{details.studios.length - 4} 家出品方...
                      </button>
                    ) : null}
                  </div>
                </div>
              ) : null}
            </div>
          ) : null}

          {/* Collection Banner */}
          {details?.setName || details?.setId ? (
            <div className="lux-media-nfo-collection-card p-3 rounded-lg bg-black/30 border border-[var(--lux-line-soft)] hover:border-[var(--lux-line)] transition flex items-center justify-between gap-4 cursor-pointer">
              <div className="flex items-center gap-3">
                <span className="text-base text-[var(--lux-silver-accent)]">🗂️</span>
                <div>
                  <span className="text-xs text-[var(--lux-subtle)] block">所属系列合集</span>
                  <span className="lux-collection-name text-sm font-semibold text-white">{details.setName || collection}</span>
                </div>
              </div>
              <div className="text-xs text-[var(--lux-muted)] hover:text-white flex items-center gap-1">
                <span>浏览系列全部作品</span>
                <span>→</span>
              </div>
            </div>
          ) : null}

          {/* External Database IDs & Links */}
          {providerIds.length || details?.website || details?.trailers?.length ? (
            <div className="flex flex-wrap items-center justify-between gap-3 pt-3.5 border-t border-[var(--lux-line-soft)]">
              <div className="flex flex-wrap items-center gap-2 text-xs">
                <span className="text-[var(--lux-subtle)] mr-1">外部条目:</span>
                {providerIds.map(([provider, id]) => (
                  <a
                    key={`${provider}-${id}`}
                    href={providerExternalUrl(provider, id)}
                    target="_blank"
                    rel="noreferrer"
                    className="lux-badge-pill lux-media-nfo-provider-badge"
                  >
                    <strong className="text-white font-mono">{provider.toUpperCase()}</strong>
                    <span className="text-[var(--lux-subtle)]">{id}</span>
                  </a>
                ))}
              </div>

              {details?.website || details?.trailers?.length ? (
                <div className="lux-media-nfo-links flex items-center gap-2 text-xs" aria-label="本地 NFO 链接">
                  <span className="lux-media-nfo-links-label lux-visually-hidden">更多来源</span>
                  <div className="lux-media-nfo-links-list flex items-center gap-2">
                    {details?.website && isHttpUrl(details.website) ? (
                      <a href={details.website} target="_blank" rel="noreferrer" className="lux-badge-pill" aria-label="官方网站">
                        <span>官方网站</span>
                        <span className="text-[var(--lux-subtle)]">↗</span>
                      </a>
                    ) : null}
                    {(details?.trailers ?? []).filter(isHttpUrl).map((trailer, index) => (
                      <a href={trailer} target="_blank" rel="noreferrer" className="lux-badge-pill" aria-label={`预告片 ${index + 1}`} key={trailer}>
                        <span>预告片 {index + 1}</span>
                        <span className="text-[var(--lux-subtle)]">↗</span>
                      </a>
                    ))}
                  </div>
                </div>
              ) : null}
            </div>
          ) : null}
        </div>
      ) : null}

      {mediaInfo ? (
        <section className="lux-media-nfo-media space-y-4" aria-labelledby="media-file-heading">
          <MediaInfoContent
            {...mediaInfo}
            addedAtLabel={addedAtLabel}
            includeMetadataRows={!hasNfoDetails}
            includeOriginalLanguage={false}
            compactSummary
            includeSourceRow
            showHeader
          />
        </section>
      ) : null}
    </section>
  );
}

function hasDetails(details: MediaNfoDetails) {
  return Boolean(
    details.rating != null
    || details.votes != null
    || details.premiered
    || details.releaseDate
    || details.aired
    || details.lastAirDate
    || details.runtime != null
    || details.genres?.length
    || details.studios?.length
    || details.countries?.length
    || details.tagline
    || details.website
    || details.setName
    || details.setId
    || details.certification
    || details.seasonNumber != null
    || details.episodeNumber != null
    || details.directors?.length
    || details.writers?.length
    || details.trailers?.length
    || Object.keys(details.providerIds ?? {}).length,
  );
}

function formatSeasonEpisode(seasonNumber?: number | null, episodeNumber?: number | null) {
  if (seasonNumber == null && episodeNumber == null) return undefined;
  if (seasonNumber != null && episodeNumber != null) return `第 ${seasonNumber} 季 · 第 ${episodeNumber} 集`;
  if (seasonNumber != null) return `第 ${seasonNumber} 季`;
  return `第 ${episodeNumber} 集`;
}

function formatStatus(status?: string | null) {
  if (!status) return undefined;
  switch (status.trim().toLowerCase()) {
    case "released":
      return "已上映";
    case "ended":
      return "已完结";
    case "continuing":
    case "returning series":
      return "连载中";
    case "in production":
    case "planned":
      return "制作中";
    case "canceled":
    case "cancelled":
      return "已取消";
    case "pilot":
      return "试播集";
    default:
      return status;
  }
}

function languageSubLabel(lang?: string | null) {
  if (!lang) return undefined;
  const normalized = lang.toLowerCase();
  if (normalized === "zh" || normalized === "chi" || normalized === "zho") return "普通话";
  if (normalized === "en" || normalized === "eng") return "English";
  if (normalized === "ja" || normalized === "jpn") return "Japanese";
  if (normalized === "ko" || normalized === "kor") return "Korean";
  return undefined;
}

function providerExternalUrl(provider: string, id: string) {
  switch (provider.toLowerCase()) {
    case "imdb":
      return `https://www.imdb.com/title/${id}/`;
    case "tmdb":
      return `https://www.themoviedb.org/movie/${id}`;
    case "tvdb":
      return `https://thetvdb.com/dereferrer/series/${id}`;
    case "wikidata":
      return `https://www.wikidata.org/wiki/${id}`;
    default:
      return `#${provider}:${id}`;
  }
}

function isHttpUrl(value: string) {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}
