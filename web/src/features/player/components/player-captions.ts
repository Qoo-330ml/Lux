import type { MediaSource, MediaStream } from "../../../lib/api/types";
import type { CaptionFormat } from "../caption-parser";

export type PlayerCaptionRenderMode = "native" | "native-inband" | "overlay";

export type PlayerRuntimeCaptionTrack = {
  id: string;
  label: string;
  language?: string;
  kind: string;
  ordinal: number;
};

export type PlayerCaptionOption = {
  streamIndex: number;
  name: string;
  label: string;
  available: boolean;
  unavailableReason?: string;
  language?: string;
  isDefault: boolean;
  isForced: boolean;
  format?: CaptionFormat;
  renderMode?: PlayerCaptionRenderMode;
  runtimeTrackId?: string;
};

export type PlayerNativeCaptionTrack = {
  id: string;
  label: string;
  language?: string;
  src: string;
};

export type PlayerOverlayCaptionSource = PlayerNativeCaptionTrack & {
  format: CaptionFormat;
};

export function playerCaptionOptions(
  source: MediaSource | undefined,
  nativeTracksSupported: boolean,
  runtimeTracks: readonly PlayerRuntimeCaptionTrack[] = [],
): PlayerCaptionOption[] {
  let embeddedTextOrdinal = 0;
  return (source?.streams ?? [])
    .filter(isSubtitleStream)
    .filter((stream): stream is MediaStream & { index: number } => Number.isInteger(stream.index) && stream.index >= 0)
    .map((stream) => {
      const format = captionFormat(stream);
      const runtimeTrack = !stream.isExternal && format ? runtimeTracks[embeddedTextOrdinal++] : undefined;
      const renderMode = runtimeTrack
        ? "native-inband"
        : stream.isExternal && format === "vtt" && nativeTracksSupported
          ? "native"
          : "overlay";
      const unavailableReason = captionUnavailableReason(source, stream, format, runtimeTrack);
      const name = captionName(stream);
      return {
        streamIndex: stream.index,
        name,
        label: captionLabel(name, stream, Boolean(unavailableReason)),
        available: !unavailableReason,
        unavailableReason,
        language: normalizedText(stream.language),
        isDefault: stream.isDefault === true,
        isForced: stream.isForced === true,
        format,
        renderMode,
        runtimeTrackId: runtimeTrack?.id,
      };
    });
}

export function defaultCaptionSelection(options: readonly PlayerCaptionOption[]) {
  return options.find((option) => option.available && option.isDefault)
    ?? options.find((option) => option.available)
    ?? null;
}

export function readPlayerRuntimeCaptionTracks(video: HTMLVideoElement): PlayerRuntimeCaptionTrack[] {
  try {
    const ownedTracks = new Set<TextTrack>();
    for (const element of Array.from(video.querySelectorAll("track"))) {
      const track = element.track;
      if (track) ownedTracks.add(track);
    }
    return Array.from(video.textTracks)
      .filter((track) => (track.kind === "subtitles" || track.kind === "captions") && !ownedTracks.has(track))
      .map((track, ordinal) => ({
        id: normalizedText(track.id) ?? `inband-caption-${ordinal}`,
        label: normalizedText(track.label) ?? `字幕轨道 ${ordinal + 1}`,
        language: normalizedText(track.language),
        kind: track.kind,
        ordinal,
      }));
  } catch {
    return [];
  }
}

export function nativeCaptionTrack(
  itemId: string,
  sourceId: string,
  option: PlayerCaptionOption | null | undefined,
): PlayerNativeCaptionTrack | null {
  if (!option?.available || option.renderMode !== "native" || !itemId || !sourceId) return null;
  return {
    id: `caption-${option.streamIndex}`,
    label: option.name,
    language: option.language,
    src: `/api/v1/items/${encodeURIComponent(itemId)}/subtitles/${option.streamIndex}?sourceId=${encodeURIComponent(sourceId)}`,
  };
}

export function overlayCaptionSource(
  itemId: string,
  sourceId: string,
  option: PlayerCaptionOption | null | undefined,
): PlayerOverlayCaptionSource | null {
  if (!option?.available || option.renderMode !== "overlay" || !option.format || !itemId || !sourceId) {
    return null;
  }
  return {
    id: `caption-${option.streamIndex}`,
    label: option.name,
    language: option.language,
    format: option.format,
    src: `/api/v1/items/${encodeURIComponent(itemId)}/subtitles/${option.streamIndex}?sourceId=${encodeURIComponent(sourceId)}`,
  };
}

function isSubtitleStream(stream: MediaStream) {
  return stream.type?.toUpperCase() === "SUBTITLE";
}

function captionUnavailableReason(
  source: MediaSource | undefined,
  stream: MediaStream,
  format: CaptionFormat | undefined,
  runtimeTrack: PlayerRuntimeCaptionTrack | undefined,
) {
  if (!format) return "当前不支持此字幕格式";
  if (stream.isExternal !== true) {
    if (runtimeTrack) return undefined;
    if (source?.sourceKind === "LOCAL_FILE" && ["srt", "ass", "ssa"].includes(format)) return undefined;
    if (source?.sourceKind === "STRM_URL") return "浏览器未暴露远程内嵌字幕";
    return "浏览器未暴露内嵌字幕";
  }
  return undefined;
}

function captionFormat(stream: MediaStream): CaptionFormat | undefined {
  const codec = stream.codec?.trim().toLowerCase();
  return codec === "srt" || codec === "subrip"
    ? "srt"
    : codec === "ass" || codec === "ssa" || codec === "vtt"
      ? codec
    : undefined;
}

function captionLabel(name: string, stream: MediaStream, unavailable: boolean) {
  const tags = [
    stream.isDefault ? "默认" : null,
    stream.isForced ? "强制" : null,
    unavailable ? "暂不支持" : null,
  ].filter((value): value is string => Boolean(value));
  return tags.length ? `${name} · ${tags.join(" · ")}` : name;
}

function captionName(stream: MediaStream) {
  return normalizedText(stream.title)
    ?? normalizedText(stream.language)
    ?? `字幕轨道 ${stream.index + 1}`;
}

function normalizedText(value: string | null | undefined) {
  const normalized = value?.trim();
  return normalized || undefined;
}
