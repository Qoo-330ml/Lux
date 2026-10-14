import { useState } from "react";
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
  zh: "中文",
  "zh-cn": "中文",
  "zh-hk": "中文",
  "zh-sg": "中文",
  "zh-tw": "中文",
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
  addedAtLabel?: string | null;
  includeMetadataRows?: boolean;
  includeOriginalLanguage?: boolean;
  includeSourceRow?: boolean;
  compactSummary?: boolean;
  collapseTechnicalDetails?: boolean;
  showHeader?: boolean;
};

export function MediaInfoPanel(props: MediaInfoPanelProps) {
  return (
    <section className="lux-media-info space-y-4" aria-labelledby="media-info-heading">
      <MediaInfoContent {...props} showHeader />
    </section>
  );
}

export function MediaInfoContent({
  source,
  itemType,
  lastAirDate,
  status,
  originalLanguage,
  addedAtLabel,
  includeMetadataRows = true,
  includeOriginalLanguage = true,
  includeSourceRow = true,
  collapseTechnicalDetails = false,
  showHeader = true,
}: MediaInfoPanelProps) {
  const [showAddress, setShowAddress] = useState(false);
  const [copied, setCopied] = useState(false);

  const streams = source.streams ?? [];
  const compactSummaryValues = [
    mediaTypeLabel(itemType),
    includeSourceRow ? (source.sourceKind === "STRM_URL" ? "STRM 网络媒体" : "本地媒体文件") : undefined,
    source.qualityLabel || source.editionName || undefined,
    addedAtLabel,
  ].filter((value): value is string => Boolean(value));

  const handleCopyUrl = () => {
    if (!source.externalUrl) return;
    navigator.clipboard.writeText(source.externalUrl).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  };

  return (
    <div className="space-y-4">
      {showHeader ? (
        <div className="flex flex-col sm:flex-row sm:items-baseline justify-between gap-2 border-b border-[var(--lux-line-soft)] pb-3">
          <div className="flex items-baseline gap-3">
            <div className="lux-media-nfo-media-heading">
              <h3 id="media-file-heading" className="text-xl md:text-2xl font-bold text-white tracking-tight">媒体文件</h3>
            </div>
            {compactSummaryValues.length ? (
              <span id="media-source-pill" className="lux-media-file-summary is-compact text-xs text-[var(--lux-subtle)]">
                {compactSummaryValues.join(" · ")}
                {includeMetadataRows ? (
                  <>
                    <InfoRow label="最后播出" value={lastAirDate ?? undefined} />
                    <InfoRow label="状态" value={status ?? undefined} />
                    {includeOriginalLanguage ? <InfoRow label="原始语言" value={originalLanguage ? languageLabel(originalLanguage) : undefined} /> : null}
                  </>
                ) : null}
              </span>
            ) : null}
          </div>
          <span id="media-stream-count-badge" className="text-xs text-[var(--lux-subtle)]">
            {streams.length ? `${streams.length} 条媒体轨` : "暂无轨道信息"}
          </span>
        </div>
      ) : null}

      {/* Quick Specs & File Facts Ribbon */}
      <div className="lux-surface-panel lux-media-file-facts p-3.5 flex flex-wrap items-center justify-between gap-4 text-xs">
        <div className="flex flex-wrap items-center gap-2.5">
          {source.container ? (
            <span id="fact-container" className="px-2 py-0.5 rounded bg-white/[0.06] text-white font-mono font-medium">
              {source.container.toUpperCase()}
            </span>
          ) : null}
          {formatBytes(source.size) ? (
            <>
              <span id="fact-size" className="text-[var(--lux-muted)]">{formatBytes(source.size)}</span>
              <span className="text-[var(--lux-subtle)]">·</span>
            </>
          ) : null}
          {formatBitrate(source.bitrate) ? (
            <>
              <span id="fact-bitrate" className="text-[var(--lux-muted)]">{formatBitrate(source.bitrate)}</span>
              <span className="text-[var(--lux-subtle)]">·</span>
            </>
          ) : null}
          {runtimeLabel(source.durationTicks) ? (
            <>
              <span id="fact-runtime" className="text-[var(--lux-muted)]">{runtimeLabel(source.durationTicks)}</span>
              <span className="text-[var(--lux-subtle)]">·</span>
            </>
          ) : null}
          {source.sourceKind ? (
            <span id="fact-source-kind" className="text-[var(--lux-muted)]">
              {source.sourceKind === "STRM_URL" ? "STRM 远程播放源" : "本地媒体文件"}
            </span>
          ) : null}
        </div>

        {source.externalUrl ? (
          <button
            type="button"
            onClick={() => setShowAddress((prev) => !prev)}
            className="lux-badge-pill hover:text-white cursor-pointer ml-auto sm:ml-0"
          >
            <span>媒体源地址与技术详情</span>
            <span id="url-drawer-chevron" className="text-[var(--lux-subtle)] transition-transform">
              {showAddress ? "▲" : "▼"}
            </span>
          </button>
        ) : null}
      </div>

      {/* Collapsible Technical URL Drawer */}
      {source.externalUrl ? (
        collapseTechnicalDetails ? (
          <details className="lux-media-info-address">
            <summary>媒体地址</summary>
            <code>{source.externalUrl}</code>
          </details>
        ) : (
          <div
            id="media-url-drawer"
            className={`lux-surface-panel p-4 space-y-2.5 bg-black/40 lux-media-info-address ${showAddress ? "" : "hidden"}`}
          >
            <div className="flex items-center justify-between text-xs">
              <span className="text-[var(--lux-subtle)]">媒体播放地址 (已安全收纳)</span>
              <button
                type="button"
                onClick={handleCopyUrl}
                className="text-xs text-[var(--lux-silver-accent)] hover:underline flex items-center gap-1 cursor-pointer"
              >
                {copied ? "已复制" : "复制地址"}
              </button>
            </div>
            <div
              id="media-url-display"
              className="p-2.5 rounded bg-black/60 border border-[var(--lux-line-soft)] font-mono text-xs text-[var(--lux-muted)] break-all select-all"
            >
              <code>{source.externalUrl}</code>
            </div>
          </div>
        )
      ) : null}

      {/* LUX NATIVE HORIZONTAL SCROLL RAIL (精修版横向排列) */}
      {streams.length ? (
        <div className="relative group">
          <HorizontalScrollRail className="lux-media-stream-rail overflow-x-auto pb-3 pt-1" ariaLabel="媒体轨道">
            <div id="lux-stream-rail" className="lux-media-stream-grid flex flex-nowrap items-stretch gap-3 min-w-full">
              {streams.map((stream, idx) => (
                <MediaStreamCard
                  key={`${stream.type ?? "stream"}-${stream.index}`}
                  stream={stream}
                  index={idx}
                  sourceBitrate={source.bitrate}
                />
              ))}
            </div>
          </HorizontalScrollRail>
        </div>
      ) : null}
    </div>
  );
}

function MediaStreamCard({
  stream,
  index,
  sourceBitrate,
}: {
  stream: MediaStream;
  index: number;
  sourceBitrate?: number | null;
}) {
  const kind = stream.type?.toUpperCase() ?? "UNKNOWN";
  const typeLabel = streamTypeLabel(kind);
  const title = stream.title || streamLabel(stream, kind);
  const rows = streamDetailRows(stream, kind, sourceBitrate, index);
  const language = stream.language ? languageLabel(stream.language) : undefined;
  const codec = stream.codec ? codecLabel(stream.codec) : undefined;

  const badges: Array<{ label: string; accent?: boolean }> = [];
  if (codec) badges.push({ label: codec });
  if (language) badges.push({ label: kind === "SUBTITLE" ? `${language}字幕` : language });
  if (stream.isDefault) badges.push({ label: "默认", accent: true });
  if (stream.isForced) badges.push({ label: "强制", accent: true });
  if (stream.isExternal) badges.push({ label: "外挂" });

  return (
    <article className="lux-media-stream-card" aria-label={`${typeLabel}轨道 ${index + 1}`}>
      {/* Card Top: Heading & Badges */}
      <div className="lux-media-stream-heading">
        <div className="lux-media-stream-heading-top flex items-center justify-between gap-2">
          <span className="lux-media-stream-type text-xs font-semibold uppercase tracking-wider text-[var(--lux-silver-accent)]">
            {typeLabel}
          </span>
          <span className="lux-media-stream-index text-[11px] font-mono text-[var(--lux-subtle)]">
            #{index + 1}
          </span>
        </div>

        <h3 className="text-sm font-bold text-white mt-1 truncate" title={title}>
          {title}
        </h3>

        <div className="lux-media-stream-badges flex flex-wrap gap-1 mt-2.5">
          {badges.map((b) => (
            <span
              key={b.label}
              className={`lux-badge-pill text-[11px] py-0 px-1.5 ${b.accent ? "lux-badge-accent is-default" : ""}`}
            >
              {b.label}
            </span>
          ))}
        </div>
      </div>

      {/* Card Bottom: Balanced Key-Value Rows */}
      {rows.length ? (
        <div className="lux-media-stream-details grid gap-1 pt-3 mt-3 border-t border-[var(--lux-line-soft)] text-xs">
          {rows.map(([label, value]) => (
            <div key={label} className="lux-media-nfo-row flex items-center justify-between gap-2 py-0.5">
              <span className="text-[var(--lux-subtle)] shrink-0">{label}</span>
              <strong className="font-mono font-medium text-[var(--lux-text)] truncate text-right">
                {value}
              </strong>
            </div>
          ))}
        </div>
      ) : null}
    </article>
  );
}

function streamDetailRows(
  stream: MediaStream,
  kind: string,
  sourceBitrate?: number | null,
  index?: number,
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
    add("画幅比例", formatAspectRatio(details.AspectRatio));
    add("帧率", formatFrameRate(details.RealFrameRate ?? details.AverageFrameRate));
    add("码率", formatBitrate(details.BitRate ?? sourceBitrate));
    add("配置", textValue(details.Profile));
    add("等级", formatCodecLevel(details.Level));
    add("色深", withUnit(details.BitDepth, "bit"));
    add("像素格式", textValue(details.PixelFormat));
    add("色域", textValue(details.ColorSpace ?? details.ColorRange));
    add("色彩转换", textValue(details.ColorTransfer));
    add("色彩基元", textValue(details.ColorPrimaries));
  }
  if (kind === "AUDIO") {
    add("布局", channelLayout(details.ChannelLayout, details.Channels));
    add("声道", withUnit(details.Channels, "ch"));
    add("采样率", formatSampleRate(details.SampleRate));
    add("配置", textValue(details.Profile));
    add("码率", formatBitrate(details.BitRate));
    const audioLang = stream.language ? (
      stream.language.toLowerCase().startsWith("zh") || stream.language === "chi" || stream.language === "zho"
        ? "中文 (Chinese)"
        : languageLabel(stream.language)
    ) : undefined;
    add("语言", audioLang);
    const isHighQuality = (numberValue(details.BitRate) ?? 0) >= 1_500_000 || (numberValue(details.SampleRate) ?? 0) >= 48_000;
    if (isHighQuality) {
      add("音质", "无损高清");
    }
  }
  if (kind === "SUBTITLE") {
    add("格式", stream.codec ? (codecLabel(stream.codec) === "PGSSUB" ? "图形位图 (PGS)" : codecLabel(stream.codec)) : undefined);
    const subLang = stream.language ? (
      stream.language.toLowerCase().startsWith("zh") || stream.language === "chi" || stream.language === "zho"
        ? "中文 (简体)"
        : stream.language.toLowerCase().startsWith("en")
        ? "英语 (English)"
        : languageLabel(stream.language)
    ) : undefined;
    add("语言", subLang);
    if (stream.language) {
      add("语言代码", stream.language === "chi" ? "chi / zho" : stream.language);
    }
    add("来源", stream.isExternal ? "同目录外挂" : "内嵌字幕轨");
    if (index !== undefined) {
      add("轨道序号", `#${index + 1}`);
    }
    add("默认", stream.isDefault ? "是" : "否");
    add("强制字幕", stream.isForced ? "是" : "否");
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

export function languageLabel(language: string) {
  return LANGUAGE_NAMES[language.toLowerCase()] ?? language;
}

function codecLabel(codec: string) {
  const normalized = codec.toLowerCase();
  if (normalized === "h264" || normalized === "avc") return "H264";
  if (normalized === "hevc" || normalized === "h265") return "HEVC";
  if (normalized === "subrip") return "SubRip";
  if (normalized === "hdmv_pgs_subtitle" || normalized === "pgssub") return "PGSSUB";
  if (normalized.includes("dts")) return "DTS-HD MA";
  if (normalized === "truehd") return "TrueHD";
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

function formatAspectRatio(value: unknown) {
  const text = textValue(value);
  if (!text) return undefined;
  const match = text.trim().match(/^(\d+(?:\.\d+)?)\s*:\s*(\d+(?:\.\d+)?)$/);
  if (!match) return text;
  const w = Number(match[1]);
  const h = Number(match[2]);
  if (!w || !h || !Number.isFinite(w) || !Number.isFinite(h)) return text;
  if (text === "16:9" || text === "4:3") return text;
  const ratio = (w / h).toFixed(2).replace(/\.00$/, "");
  if (ratio === "2.55" || ratio === "2.40" || ratio === "2.35") {
    return `${ratio}:1 (超宽银幕)`;
  }
  return `${ratio}:1`;
}

function formatCodecLevel(value: unknown) {
  const text = textValue(value);
  if (!text) return undefined;
  const num = Number(text);
  if (Number.isFinite(num) && num >= 10 && num <= 70 && !text.includes(".")) {
    return (num / 10).toFixed(1).replace(/\.0$/, "");
  }
  return text;
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
  if (text) {
    if (text === "5.1") return "5.1 环绕声";
    if (text === "7.1") return "7.1 环绕声";
    return text;
  }
  const count = numberValue(channels);
  if (count === undefined) return undefined;
  const knownLayouts: Record<number, string> = {
    1: "1.0",
    2: "2.0",
    3: "2.1",
    4: "4.0",
    5: "5.0",
    6: "5.1 环绕声",
    7: "6.1 环绕声",
    8: "7.1 环绕声",
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
