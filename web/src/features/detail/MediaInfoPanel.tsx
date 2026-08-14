import type { MediaSource, MediaStream } from "../../lib/api/types";
import { HorizontalScrollRail } from "../../components/layout/HorizontalScrollRail";
import { mediaTypeLabel, runtimeLabel } from "../home/media";

const LANGUAGE_NAMES: Record<string, string> = {
  en: "英语",
  chi: "中文",
  zho: "中文",
  eng: "英语",
  jpn: "日语",
  kor: "韩语",
  fre: "法语",
  fra: "法语",
  spa: "西班牙语",
  und: "未知语言",
};

export type MediaInfoPanelProps = {
  source: MediaSource;
  itemType?: string | null;
  lastAirDate?: string | null;
  status?: string | null;
  originalLanguage?: string | null;
};

export function MediaInfoPanel(props: MediaInfoPanelProps) {
  return (
    <section className="lux-media-info" aria-labelledby="media-info-heading">
      <div className="lux-media-info-heading">
        <h2 id="media-info-heading">媒体信息</h2>
        <span>{props.source.streams?.length ? `${props.source.streams.length} 条媒体轨` : "暂无轨道信息"}</span>
      </div>
      <MediaInfoContent {...props} />
    </section>
  );
}

export function MediaInfoContent({
  source,
  itemType,
  lastAirDate,
  status,
  originalLanguage,
}: MediaInfoPanelProps) {
  const streams = source.streams ?? [];
  const streamGroups = ["VIDEO", "AUDIO", "SUBTITLE"]
    .map((type) => ({ type, streams: streams.filter((stream) => stream.type?.toUpperCase() === type) }))
    .filter((group) => group.streams.length);

  return (
    <>
      <div className="lux-media-nfo-summary">
        <InfoRow label="类型" value={mediaTypeLabel(itemType)} />
        <InfoRow label="最后播出" value={lastAirDate ?? undefined} />
        <InfoRow label="状态" value={status ?? undefined} />
        <InfoRow label="原始语言" value={originalLanguage ? languageLabel(originalLanguage) : undefined} />
        <InfoRow label="来源" value={source.sourceKind === "STRM_URL" ? "STRM 网络媒体" : "本地媒体文件"} />
        <InfoRow label="版本" value={source.qualityLabel || source.editionName || undefined} />
      </div>
      {source.externalUrl ? (
        <div className="lux-media-nfo-row lux-media-info-address">
          <span>媒体地址</span>
          <code>{source.externalUrl}</code>
        </div>
      ) : null}
      <div className="lux-media-nfo-tags" aria-label="媒体文件摘要">
        {source.container ? <span>{source.container.toUpperCase()}</span> : null}
        {formatBytes(source.size) ? <span>{formatBytes(source.size)}</span> : null}
        {formatBitrate(source.bitrate) ? <span>{formatBitrate(source.bitrate)}</span> : null}
        {runtimeLabel(source.durationTicks) ? <span>{runtimeLabel(source.durationTicks)}</span> : null}
      </div>
      {streamGroups.length ? (
        <HorizontalScrollRail className="lux-media-stream-rail" ariaLabel="视频轨道">
          <div className="lux-media-stream-grid">
            {streamGroups.flatMap(({ type, streams: groupStreams }) => groupStreams.map((stream) => (
              <MediaStreamCard
                key={`${type}-${stream.index}`}
                stream={stream}
                sourceBitrate={source.bitrate}
              />
            )))}
          </div>
        </HorizontalScrollRail>
      ) : null}
    </>
  );
}

function MediaStreamCard({
  stream,
  sourceBitrate,
}: {
  stream: MediaStream;
  sourceBitrate?: number | null;
}) {
  const kind = stream.type?.toUpperCase() ?? "UNKNOWN";
  const title = stream.title || streamLabel(stream, kind);
  const rows = streamDetailRows(stream, kind, sourceBitrate);
  const language = stream.language ? languageLabel(stream.language) : undefined;

  return (
    <article className="lux-media-stream-card" aria-label={`${streamTypeLabel(kind)}轨道 ${stream.index + 1}`}>
      <div className="lux-media-stream-heading">
        <span className="lux-media-stream-type">{streamTypeLabel(kind)}</span>
        <h3>{title}</h3>
      </div>
      <div className="lux-media-stream-badges">
        {stream.codec ? <span>{codecLabel(stream.codec)}</span> : null}
        {language ? <span>{kind === "SUBTITLE" ? `${language}字幕` : language}</span> : null}
        {stream.isDefault ? <span>默认</span> : null}
        {stream.isForced ? <span>强制</span> : null}
        {stream.isExternal ? <span>外挂</span> : null}
      </div>
      {rows.length ? (
        <div className="lux-media-stream-details">
          {rows.map(([label, value]) => <InfoRow key={label} label={label} value={value} />)}
        </div>
      ) : null}
    </article>
  );
}

function streamDetailRows(
  stream: MediaStream,
  kind: string,
  sourceBitrate?: number | null,
): Array<[string, string]> {
  const details = stream.details ?? {};
  const rows: Array<[string, string]> = [];
  const add = (label: string, value: string | undefined) => {
    if (value) rows.push([label, value]);
  };

  if (kind === "VIDEO") {
    const width = numberValue(details.Width);
    const height = numberValue(details.Height);
    add("分辨率", width && height ? `${width} × ${height}` : undefined);
    add("宽高比", textValue(details.AspectRatio));
    add("帧率", formatFrameRate(details.RealFrameRate ?? details.AverageFrameRate));
    add("码率", formatBitrate(details.BitRate ?? sourceBitrate));
    add("配置", textValue(details.Profile));
    add("等级", textValue(details.Level));
    add("色深", withUnit(details.BitDepth, "bit"));
    add("像素格式", textValue(details.PixelFormat));
    add("色域", textValue(details.ColorSpace ?? details.ColorRange));
    add("色彩转换", textValue(details.ColorTransfer));
    add("色彩基元", textValue(details.ColorPrimaries));
  }
  if (kind === "AUDIO") {
    add("布局", channelLayout(details.ChannelLayout, details.Channels));
    add("声道", withUnit(details.Channels, "ch"));
    add("码率", formatBitrate(details.BitRate));
    add("采样率", formatSampleRate(details.SampleRate));
    add("配置", textValue(details.Profile));
  }
  return rows;
}

function streamLabel(stream: MediaStream, kind: string) {
  return `${streamTypeLabel(kind)}轨道 ${stream.index + 1}`;
}

function streamTypeLabel(type: string) {
  switch (type) {
    case "VIDEO": return "视频";
    case "AUDIO": return "音频";
    case "SUBTITLE": return "字幕";
    default: return "媒体";
  }
}

function languageLabel(language: string) {
  return LANGUAGE_NAMES[language.toLowerCase()] ?? language;
}

function codecLabel(codec: string) {
  const normalized = codec.toLowerCase();
  if (normalized === "h264" || normalized === "avc") return "H264";
  if (normalized === "hevc" || normalized === "h265") return "HEVC";
  if (normalized === "subrip") return "SubRip";
  if (normalized === "hdmv_pgs_subtitle" || normalized === "pgssub") return "PGSSUB";
  return codec.toUpperCase();
}

function textValue(value: unknown) {
  return typeof value === "string" || typeof value === "number" ? String(value) : undefined;
}

function numberValue(value: unknown) {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim() && Number.isFinite(Number(value))) return Number(value);
  return undefined;
}

function withUnit(value: unknown, unit: string) {
  const number = numberValue(value);
  return number === undefined ? undefined : `${number} ${unit}`;
}

function formatFrameRate(value: unknown) {
  const number = ratioValue(value) ?? numberValue(value);
  return number === undefined ? undefined : `${number} fps`;
}

function ratioValue(value: unknown) {
  if (typeof value !== "string") return undefined;
  const match = value.trim().match(/^(\d+(?:\.\d+)?)\s*\/\s*(\d+(?:\.\d+)?)$/);
  if (!match) return undefined;
  const numerator = Number(match[1]);
  const denominator = Number(match[2]);
  if (!Number.isFinite(numerator) || !Number.isFinite(denominator) || denominator === 0) return undefined;
  return Number(stripTrailingZeros(numerator / denominator));
}

function channelLayout(layout: unknown, channels: unknown) {
  const text = textValue(layout);
  if (text) return text;
  const count = numberValue(channels);
  if (count === undefined) return undefined;
  const knownLayouts: Record<number, string> = {
    1: "1.0",
    2: "2.0",
    3: "2.1",
    4: "4.0",
    5: "5.0",
    6: "5.1",
    7: "6.1",
    8: "7.1",
  };
  return knownLayouts[count] ?? `${count} ch`;
}

function formatSampleRate(value: unknown) {
  const number = numberValue(value);
  return number === undefined ? undefined : `${stripTrailingZeros(number / 1000)} kHz`;
}

function formatBitrate(value: unknown) {
  const number = numberValue(value);
  if (number === undefined) return undefined;
  if (number >= 1_000_000) return `${stripTrailingZeros(number / 1_000_000)} Mbps`;
  if (number >= 1_000) return `${stripTrailingZeros(number / 1_000)} kbps`;
  return `${number} bps`;
}

function formatBytes(value: number | null | undefined) {
  if (!value || value < 0) return undefined;
  if (value >= 1024 ** 3) return `${stripTrailingZeros(value / 1024 ** 3)} GB`;
  if (value >= 1024 ** 2) return `${stripTrailingZeros(value / 1024 ** 2)} MB`;
  if (value >= 1024) return `${stripTrailingZeros(value / 1024)} KB`;
  return `${value} B`;
}

function stripTrailingZeros(value: number) {
  return value.toFixed(2).replace(/(\.\d*?)0+$/, "$1").replace(/\.$/, "");
}

function InfoRow({ label, value }: { label: string; value?: string }) {
  if (!value) return null;
  return <div className="lux-media-nfo-row"><span>{label}</span><strong>{value}</strong></div>;
}
