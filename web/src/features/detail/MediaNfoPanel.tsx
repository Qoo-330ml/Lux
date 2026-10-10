import { CalendarDays, ExternalLink, Globe2, Languages, Link2, UsersRound } from "lucide-react";
import type { MediaNfoCredit, MediaNfoDetails } from "../../lib/api/types";
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
  const hasNfoDetails = Boolean(details && hasDetails(details));
  if (!hasNfoDetails && !mediaInfo) return null;

  const genreTags = details?.genres ?? [];
  const collection = [
    details?.setName,
    details?.setId ? `ID ${details.setId}` : undefined,
  ].filter((value): value is string => Boolean(value)).join(" · ");
  const taxonomyGroups = [
    { label: "国家/地区", value: details?.countries?.join(" · ") },
    { label: "制片公司", value: details?.studios?.join(" · ") },
    { label: "分级", value: details?.certification },
    { label: "合集", value: collection || undefined },
  ].filter((group): group is { label: string; value: string } => Boolean(group.value));
  const providerIds = Object.entries(details?.providerIds ?? {});
  const headingSource = hasNfoDetails ? "来自本地 NFO" : "媒体技术信息";
  const lastAirDate = details?.lastAirDate ?? mediaInfo?.lastAirDate;
  const status = details?.status ?? mediaInfo?.status;
  const originalLanguage = details?.originalLanguage ?? mediaInfo?.originalLanguage;
  const hasRating = details?.rating != null;
  const identityRows: Array<{ label: string; value?: string | null; icon?: React.ReactNode }> = [
    { label: "原始片名", value: originalTitle, icon: <Globe2 size={14} /> },
    { label: "年份", value: productionYear != null ? String(productionYear) : undefined, icon: <CalendarDays size={14} /> },
    { label: "原始语言", value: originalLanguage ? languageLabel(originalLanguage) : undefined, icon: <Languages size={14} /> },
  ].filter((row) => Boolean(row.value));
  const summaryRows: Array<{ label: string; value?: string | null; icon?: React.ReactNode }> = [
    ...(!hasRating && details?.votes != null ? [{ label: "投票数", value: `${details.votes} 票` }] : []),
    { label: "首播日期", value: details?.premiered, icon: <CalendarDays size={14} /> },
    { label: "发行日期", value: details?.releaseDate, icon: <CalendarDays size={14} /> },
    { label: "播出日期", value: details?.aired, icon: <CalendarDays size={14} /> },
    { label: "最后播出", value: lastAirDate, icon: <CalendarDays size={14} /> },
    { label: "运行时长", value: details?.runtime != null ? `${details.runtime} 分钟` : undefined },
    { label: "季 / 集", value: formatSeasonEpisode(details?.seasonNumber, details?.episodeNumber) },
    { label: "状态", value: status },
  ].filter((row) => Boolean(row.value));

  return (
    <section className="lux-media-nfo" aria-labelledby="media-nfo-heading">
      <div className="lux-media-nfo-heading">
        <h2 id="media-nfo-heading">更多信息</h2>
        <span>{headingSource}</span>
      </div>
      {(genreTags.length > 0 || taxonomyGroups.length > 0) ? (
        <div className="lux-media-nfo-taxonomy">
          {genreTags.length ? (
            <div className="lux-media-nfo-genre-tags" aria-label="类型">
              {genreTags.map((genre) => <span key={genre}>{genre}</span>)}
            </div>
          ) : null}
          {taxonomyGroups.length ? (
            <div className="lux-media-nfo-taxonomy-groups">
              {taxonomyGroups.map(({ label, value }) => (
                <div key={label} className="lux-media-nfo-taxonomy-group">
                  <span>{label}</span><strong>{value}</strong>
                </div>
              ))}
            </div>
          ) : null}
        </div>
      ) : null}
      {details?.tagline ? <p className="lux-media-nfo-tagline">“{details.tagline}”</p> : null}
      {hasNfoDetails && (hasRating || summaryRows.length > 0) ? (
        <div className="lux-media-nfo-grid">
          <div className={`lux-media-nfo-overview${hasRating && summaryRows.length ? " has-rating" : ""}`}>
            {hasRating ? (
              <div className="lux-media-nfo-rating" aria-label="评分与投票数">
                <span>评分</span>
                {details?.rating != null ? <strong>{details.rating}<small> / 10</small></strong> : null}
                {details?.votes != null ? <span className="lux-media-nfo-votes">{details.votes} 票</span> : null}
              </div>
            ) : null}
            {summaryRows.length ? (
              <div className="lux-media-nfo-summary">
                {summaryRows.map(({ label, value, icon }) => <NfoRow key={label} label={label} value={value} icon={icon} />)}
              </div>
            ) : null}
          </div>
        </div>
      ) : null}
      {identityRows.length ? (
        <div className="lux-media-nfo-identity" aria-label="影片信息">
          {identityRows.map(({ label, value, icon }) => <IdentityItem key={label} label={label} value={value} icon={icon} />)}
        </div>
      ) : null}
      {details?.directors?.length || details?.writers?.length || providerIds.length ? (
        <div className="lux-media-nfo-secondary" aria-label="制作与来源信息">
          {details?.directors?.length ? <CreditItem label="导演" credits={details.directors} /> : null}
          {details?.writers?.length ? <CreditItem label="编剧" credits={details.writers} /> : null}
          {providerIds.length ? (
            <div className="lux-media-nfo-secondary-item lux-media-nfo-provider-item">
              <span><Link2 size={14} />外部 ID</span>
              <strong className="lux-media-nfo-provider-ids">
                {providerIds.map(([provider, id]) => <span key={`${provider}-${id}`}>{provider.toUpperCase()} {id}</span>)}
              </strong>
            </div>
          ) : null}
        </div>
      ) : null}
      {details?.website || details?.trailers?.length ? (
        <div className="lux-media-nfo-links" aria-label="本地 NFO 链接">
          <span className="lux-media-nfo-links-label">更多来源</span>
          <div className="lux-media-nfo-links-list">
            {details?.website && isHttpUrl(details.website) ? (
              <a href={details.website} target="_blank" rel="noreferrer" aria-label="官方网站">
                <ExternalLink size={14} /> 官方网站
              </a>
            ) : null}
            {(details?.trailers ?? []).filter(isHttpUrl).map((trailer, index) => (
              <a href={trailer} target="_blank" rel="noreferrer" aria-label={`预告片 ${index + 1}`} key={trailer}>
                <ExternalLink size={14} /> 预告片 {index + 1}
              </a>
            ))}
          </div>
        </div>
      ) : null}
      {mediaInfo ? (
        <section className="lux-media-nfo-media" aria-labelledby="media-file-heading">
          <div className="lux-media-nfo-media-heading">
            <h3 id="media-file-heading">媒体文件</h3>
          </div>
          <MediaInfoContent
            {...mediaInfo}
            addedAtLabel={addedAtLabel}
            includeMetadataRows={!hasNfoDetails}
            includeOriginalLanguage={false}
            compactSummary
            includeSourceRow
          />
        </section>
      ) : null}
    </section>
  );
}

function CreditItem({ label, credits }: { label: string; credits: MediaNfoCredit[] }) {
  return (
    <div className="lux-media-nfo-secondary-item">
      <span><UsersRound size={14} />{label}</span>
      <strong>{credits.map((credit) => credit.name).join("、")}</strong>
    </div>
  );
}

function IdentityItem({ label, value, icon }: { label: string; value?: string | null; icon?: React.ReactNode }) {
  if (!value) return null;
  return (
    <div className="lux-media-nfo-identity-item">
      <span>{icon}{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function NfoRow({ label, value, icon }: { label: string; value?: string | null; icon?: React.ReactNode }) {
  if (!value) return null;
  return <div className="lux-media-nfo-row"><span>{icon}{label}</span><strong>{value}</strong></div>;
}

function hasDetails(details: MediaNfoDetails) {
  return Boolean(
    details.rating != null || details.tagline || details.votes != null || details.premiered || details.releaseDate || details.aired
      || details.lastAirDate || details.runtime != null || details.seasonNumber != null || details.episodeNumber != null
      || details.status || details.originalLanguage || details.website || details.setName || details.setId
      || details.certification || details.genres?.length || details.countries?.length
      || details.studios?.length || details.directors?.length || details.writers?.length
      || Object.keys(details.providerIds ?? {}).length || details.trailers?.length,
  );
}

function formatSeasonEpisode(season?: number | null, episode?: number | null) {
  if (season == null && episode == null) return undefined;
  if (season == null) return `第 ${episode} 集`;
  if (episode == null) return `第 ${season} 季`;
  return `第 ${season} 季 · 第 ${episode} 集`;
}

function isHttpUrl(value: string) {
  return value.startsWith("https://") || value.startsWith("http://");
}
