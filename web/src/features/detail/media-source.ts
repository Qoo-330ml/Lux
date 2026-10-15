import type { MediaSource, MediaStream } from "../../lib/api/types";
/** Parts of one version (cd1, cd2 ...) are not alternative versions: list only the first part. */
export function versionSelectableSources(sources: MediaSource[]) {
  return sources.filter(
    (source) =>
      source.partIndex == null ||
      !sources.some(
        (other) =>
          other.versionKey === source.versionKey &&
          other.partIndex != null &&
          other.partIndex < source.partIndex!,
      ),
  );
}

export function sourceLabel(source: MediaSource, index: number) {
  const videoStream = source.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  const qualityTokens = splitSourceInfo(source.qualityLabel);
  const qualityRange = qualityTokens.find(dynamicRangeToken);
  const labels = uniqueSourceLabels([
    ...qualityTokens.filter((token) => !dynamicRangeToken(token)),
    videoCodecLabel(videoStream),
    dynamicRangeLabel(videoStream) ?? qualityRange,
    bitDepthLabel(videoStream),
  ]);
  return labels.join(" · ") || source.editionName || `${source.container?.toUpperCase() || "视频"} · 版本 ${index + 1}`;
}

export function streamsOfType(source: MediaSource | undefined, type: "AUDIO" | "SUBTITLE") {
  return source?.streams?.filter((stream) => isTrackType(stream, type)) ?? [];
}

function isTrackType(stream: MediaStream, type: "AUDIO" | "SUBTITLE") {
  return stream.type?.toUpperCase() === type;
}

export function defaultStreamIndex(source: MediaSource | undefined, type: "AUDIO" | "SUBTITLE") {
  const streams = streamsOfType(source, type);
  const stream = streams.find((candidate) => candidate.isDefault) ?? streams[0];
  return stream ? String(stream.index) : undefined;
}

export function streamLabel(stream: MediaStream, typeLabel: string, index: number) {
  const labels = uniqueSourceLabels([
    stream.title ?? undefined,
    stream.language ? languageLabel(stream.language) : undefined,
    stream.codec?.trim().toUpperCase(),
  ]);
  return labels.join(" · ") || `${typeLabel}轨道 ${index + 1}`;
}

function languageLabel(value: string) {
  const normalized = value.trim().toLowerCase();
  return ({
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
    und: "未知语言",
  } as Record<string, string>)[normalized] ?? value;
}

function splitSourceInfo(value?: string | null) {
  return value?.split(/\s*[·•|/,]\s*|\s+/).map((part) => part.trim()).filter(Boolean) ?? [];
}

function dynamicRangeToken(value: string) {
  const normalized = value.toLowerCase().replace(/[\s_-]+/g, "");
  if (normalized === "sdr") return "SDR";
  if (normalized === "hdr10+") return "HDR10+";
  if (normalized === "hdr10") return "HDR10";
  if (normalized === "hdr") return "HDR";
  if (normalized === "hlg") return "HLG";
  if (normalized === "dv" || normalized === "dovi" || normalized === "dolbyvision") return "Dolby Vision";
  return undefined;
}

function videoCodecLabel(stream?: MediaStream) {
  const codec = stream?.codec?.trim();
  if (!codec) return undefined;
  const normalized = codec.toLowerCase().replace(/[^a-z0-9]+/g, "");
  if (normalized === "hevc" || normalized === "h265" || normalized === "x265") return "HEVC";
  if (normalized === "h264" || normalized === "avc" || normalized === "x264") return "H.264";
  if (normalized === "mpeg4") return "MPEG-4";
  if (normalized === "av1") return "AV1";
  if (normalized === "vp9") return "VP9";
  if (normalized === "vp8") return "VP8";
  return codec.toUpperCase();
}

function dynamicRangeLabel(stream?: MediaStream) {
  if (!stream) return undefined;
  const detailText = Object.entries(stream.details ?? {})
    .filter(([key]) => /videorange|extendedvideotype|extendedvideosubtype|colortransfer|colorprimaries/i.test(key))
    .map(([, value]) => String(value))
    .join(" ");
  const value = `${stream.title ?? ""} ${detailText}`.toLowerCase().replace(/[_-]+/g, " ");
  if (/\bdolby\s+vision\b|\bdovi\b|\bdv\b/.test(value)) return "Dolby Vision";
  if (/\bhdr10\+?\b|\bhdr10plus\b/.test(value)) return value.includes("hdr10+") || value.includes("hdr10plus") ? "HDR10+" : "HDR10";
  if (/\bhdr\b|\bsmpte\s*2084\b|\bpq\b/.test(value)) return "HDR";
  if (/\bhlg\b|arib\s+std\s+b67/.test(value)) return "HLG";
  if (/\bsdr\b/.test(value)) return "SDR";
  return undefined;
}

function bitDepthLabel(stream?: MediaStream) {
  const rawValue = stream?.details?.BitDepth;
  if (rawValue === undefined || rawValue === null) return undefined;
  const match = String(rawValue).match(/\d+/);
  const bitDepth = match ? Number(match[0]) : Number.NaN;
  return Number.isFinite(bitDepth) && bitDepth > 0 ? `${bitDepth}-bit` : undefined;
}

function uniqueSourceLabels(labels: Array<string | undefined>) {
  const seen = new Set<string>();
  return labels.filter((label): label is string => {
    const trimmed = label?.trim();
    if (!trimmed) return false;
    const key = trimmed.toLowerCase().replace(/[\s.·-]+/g, "");
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

type SourceBadge = {
  key: string;
  label: string;
  className?: string;
  title?: string;
};

function audioBadgeLabel(stream?: MediaStream) {
  if (!stream) return undefined;
  const codec = stream.codec?.trim().toUpperCase();
  const title = stream.title?.trim() ?? "";
  if (/atmos/i.test(title)) {
    return "Atmos";
  }
  const rawChannels = stream.details?.Channels;
  const channels = typeof rawChannels === "number" ? rawChannels : Number(rawChannels);
  const channelText = channels === 8 ? "7.1" : channels === 6 ? "5.1" : channels === 2 ? "2.0" : undefined;

  if (codec === "TRUEHD") return channelText ? `TrueHD ${channelText}` : "TrueHD";
  if (codec === "DTS" || codec === "DCA") {
    if (/dts-hd\s*ma/i.test(title)) return channelText ? `DTS-HD MA ${channelText}` : "DTS-HD MA";
    return channelText ? `DTS ${channelText}` : "DTS";
  }
  if (codec === "EAC3") return channelText ? `E-AC3 ${channelText}` : "E-AC3";
  if (codec === "AC3") return channelText ? `AC3 ${channelText}` : "AC3";
  if (codec === "AAC") return channelText ? `AAC ${channelText}` : "AAC";
  if (codec === "FLAC") return channelText ? `FLAC ${channelText}` : "FLAC";
  return codec ? (channelText ? `${codec} ${channelText}` : codec) : undefined;
}

export function sourceBadges(source?: MediaSource, selectedAudioStreamIndex?: string): SourceBadge[] {
  if (!source) return [];
  const videoStream = source.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
  const audioStreams = streamsOfType(source, "AUDIO");
  const activeAudioStream = audioStreams.find((stream) => String(stream.index) === selectedAudioStreamIndex) ?? audioStreams[0];

  const qualityTokens = splitSourceInfo(source.qualityLabel);
  const qualityRange = qualityTokens.find(dynamicRangeToken);
  const rawResolution = qualityTokens.find((token) => !dynamicRangeToken(token)) || source.qualityLabel?.trim();
  const codec = videoCodecLabel(videoStream);
  const range = dynamicRangeLabel(videoStream) ?? qualityRange;
  const bitDepth = bitDepthLabel(videoStream);
  const audio = audioBadgeLabel(activeAudioStream);

  const badges: SourceBadge[] = [];

  if (rawResolution) {
    const is4k = /4k|2160/i.test(rawResolution);
    const is1080p = /1080/i.test(rawResolution);
    badges.push({
      key: "resolution",
      label: is4k ? "4K UHD" : is1080p ? "1080P" : rawResolution.toUpperCase(),
      className: is4k ? "is-4k" : is1080p ? "is-1080p" : undefined,
      title: "分辨率",
    });
  }

  if (codec) {
    badges.push({
      key: "codec",
      label: codec,
      title: "视频编码",
    });
  }

  if (range) {
    const isHdr = /hdr|dolby|dovi|hlg/i.test(range);
    badges.push({
      key: "range",
      label: range,
      className: isHdr ? "is-hdr" : undefined,
      title: "动态范围",
    });
  }

  if (bitDepth) {
    badges.push({
      key: "bitDepth",
      label: bitDepth,
      title: "色深",
    });
  }

  if (audio) {
    badges.push({
      key: "audio",
      label: audio,
      className: "is-audio",
      title: "音频格式",
    });
  }

  return badges;
}
