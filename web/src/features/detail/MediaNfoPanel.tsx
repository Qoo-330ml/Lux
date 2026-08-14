import { CalendarDays, ExternalLink, Link2, UsersRound } from "lucide-react";
import type { MediaNfoCredit, MediaNfoDetails } from "../../lib/api/types";
import { MediaInfoContent, languageLabel, type MediaInfoPanelProps } from "./MediaInfoPanel";

export function MediaNfoPanel({
  details,
  mediaInfo,
}: {
  details?: MediaNfoDetails | null;
  mediaInfo?: MediaInfoPanelProps;
}) {
  const hasNfoDetails = Boolean(details && hasDetails(details));
  if (!hasNfoDetails && !mediaInfo) return null;

  const tags = [
    ...(details?.genres ?? []).map((value) => ({ label: "类型", value })),
    ...(details?.countries ?? []).map((value) => ({ label: "国家/地区", value })),
    ...(details?.studios ?? []).map((value) => ({ label: "制片公司", value })),
    ...(details?.certification ? [{ label: "分级", value: details.certification }] : []),
    ...(details?.setName ? [{ label: "合集", value: details.setName }] : []),
  ];
  const providerIds = Object.entries(details?.providerIds ?? {});
  const mediaInfoStreams = mediaInfo?.source.streams ?? [];
  const headingSource = hasNfoDetails ? "来自本地 NFO" : "媒体技术信息";
  const headingSuffix = mediaInfoStreams.length ? ` · ${mediaInfoStreams.length} 条媒体轨` : "";
  const lastAirDate = details?.lastAirDate ?? mediaInfo?.lastAirDate;
  const status = details?.status ?? mediaInfo?.status;
  const originalLanguage = details?.originalLanguage ?? mediaInfo?.originalLanguage;

  return (
    <section className="lux-media-nfo" aria-labelledby="media-nfo-heading">
      <div className="lux-media-nfo-heading">
        <h2 id="media-nfo-heading">更多信息</h2>
        <span>{headingSource}{headingSuffix}</span>
      </div>
      {details?.tagline ? <p className="lux-media-nfo-tagline">“{details.tagline}”</p> : null}
      <div className="lux-media-nfo-grid">
        {tags.length ? (
          <div className="lux-media-nfo-tags" aria-label="本地元数据标签">
            {tags.map(({ label, value }, index) => <span key={`${label}-${value}-${index}`} title={label}>{value}</span>)}
          </div>
        ) : null}
        {hasNfoDetails ? (
          <div className="lux-media-nfo-summary">
            <NfoRow label="评分" value={details?.rating != null ? `${details.rating} / 10` : undefined} />
            <NfoRow label="投票数" value={details?.votes != null ? `${details.votes} 票` : undefined} />
            <NfoRow label="首播日期" value={details?.premiered} icon={<CalendarDays size={14} />} />
            <NfoRow label="发行日期" value={details?.releaseDate} icon={<CalendarDays size={14} />} />
            <NfoRow label="播出日期" value={details?.aired} icon={<CalendarDays size={14} />} />
            <NfoRow label="最后播出" value={lastAirDate} icon={<CalendarDays size={14} />} />
            <NfoRow label="运行时长" value={details?.runtime != null ? `${details.runtime} 分钟` : undefined} />
            <NfoRow label="季 / 集" value={formatSeasonEpisode(details?.seasonNumber, details?.episodeNumber)} />
            <NfoRow label="状态" value={status} />
            <NfoRow label="原始语言" value={originalLanguage ? languageLabel(originalLanguage) : undefined} />
            <NfoRow label="合集 ID" value={details?.setId} />
          </div>
        ) : null}
        {details?.directors?.length ? <CreditRow label="导演" credits={details.directors} /> : null}
        {details?.writers?.length ? <CreditRow label="编剧" credits={details.writers} /> : null}
        {providerIds.length ? (
          <NfoRow
            label="外部 ID"
            value={providerIds.map(([provider, id]) => `${provider.toUpperCase()} ${id}`).join(" · ")}
            icon={<Link2 size={14} />}
          />
        ) : null}
        {mediaInfo ? (
          <MediaInfoContent
            {...mediaInfo}
            includeMetadataRows={!hasNfoDetails}
          />
        ) : null}
      </div>
      {details?.website || details?.trailers?.length ? (
        <div className="lux-media-nfo-links" aria-label="本地 NFO 链接">
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
      ) : null}
    </section>
  );
}

function CreditRow({ label, credits }: { label: string; credits: MediaNfoCredit[] }) {
  return (
    <div className="lux-media-nfo-credit-row">
      <span><UsersRound size={14} /> {label}</span>
      <strong>{credits.map((credit) => credit.name).join("、")}</strong>
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
