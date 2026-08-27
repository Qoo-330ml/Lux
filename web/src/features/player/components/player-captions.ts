import type { MediaSource, MediaStream } from "../../../lib/api/types";
import type { CaptionFormat } from "../caption-parser";

export type PlayerCaptionRenderMode = "native" | "overlay";

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
): PlayerCaptionOption[] {
  return (source?.streams ?? [])
    .filter(isSubtitleStream)
    .filter((stream): stream is MediaStream & { index: number } => Number.isInteger(stream.index) && stream.index >= 0)
    .map((stream) => {
      const format = captionFormat(stream);
      const renderMode = format === "vtt" && nativeTracksSupported ? "native" : "overlay";
      const unavailableReason = captionUnavailableReason(stream, format);
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
      };
    });
}

export function defaultCaptionSelection(options: readonly PlayerCaptionOption[]) {
  return options.find((option) => option.available && option.isDefault)
    ?? options.find((option) => option.available)
    ?? null;
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

function captionUnavailableReason(stream: MediaStream, format: CaptionFormat | undefined) {
  if (stream.isExternal !== true) return "内嵌字幕将在后续支持";
  if (!format) return "当前不支持此字幕格式";
  return undefined;
}

function captionFormat(stream: MediaStream): CaptionFormat | undefined {
  const codec = stream.codec?.trim().toLowerCase();
  return codec === "srt" || codec === "ass" || codec === "ssa" || codec === "vtt"
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
