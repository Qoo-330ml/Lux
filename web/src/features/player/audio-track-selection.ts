import type { MediaSource, MediaStream } from "../../lib/api/types";
import {
  audioChannelLabel,
  streamsOfType,
  streamLabel,
} from "../detail/media-source";

export type AudioTrackOption = {
  /** Stream index exactly as it is written to `audioStreamIndex`. */
  index: string;
  /** Human readable label built from the shared detail-page helpers. */
  label: string;
  language: string | null;
  codec: string | null;
  channels: string | null;
  isDefault: boolean;
  isCurrent: boolean;
};

function audioStreams(source: MediaSource | undefined): MediaStream[] {
  return streamsOfType(source, "AUDIO");
}

/**
 * The index the player is actually using. Without an explicit `audioStreamIndex`
 * the server maps the first audio stream (`0:a:0`), not the stream flagged as
 * default, so the first audio stream is the fallback instead of `isDefault`.
 */
export function resolveSelectedAudioTrackIndex(
  source: MediaSource | undefined,
  requestedIndex: string | null | undefined,
): string | null {
  const streams = audioStreams(source);
  if (streams.length === 0) return null;
  const requested = requestedIndex?.trim();
  if (requested && streams.some((stream) => String(stream.index) === requested)) {
    return requested;
  }
  return String(streams[0].index);
}

export function audioTrackLabel(stream: MediaStream, position: number): string {
  const parts = [
    streamLabel(stream, "音频", position),
    audioChannelLabel(stream),
    stream.isDefault ? "默认" : undefined,
  ].filter((value): value is string => Boolean(value?.trim()));
  return parts.join(" · ");
}

export function audioTrackOptions(
  source: MediaSource | undefined,
  selectedIndex: string | null | undefined,
): AudioTrackOption[] {
  const selected = selectedIndex?.trim() || null;
  return audioStreams(source).map((stream, position) => {
    const index = String(stream.index);
    return {
      index,
      label: audioTrackLabel(stream, position),
      language: stream.language?.trim() || null,
      codec: stream.codec?.trim().toUpperCase() || null,
      channels: audioChannelLabel(stream) ?? null,
      isDefault: Boolean(stream.isDefault),
      isCurrent: selected !== null && index === selected,
    };
  });
}

export function shouldShowAudioTrackSelector(options: readonly AudioTrackOption[]): boolean {
  return options.length > 1;
}

/** Position of the audio selection in both playback query key layouts. */
export function audioParamFromQueryKey(key: readonly unknown[]): string {
  const value = key[3];
  return typeof value === "string" ? value : "default";
}

/**
 * STRM Direct Play ignores `audioStreamIndex`, so the player must not offer a
 * selection that cannot change the audible track.
 */
export function supportsAudioTrackSwitching(source: MediaSource | undefined): boolean {
  return source?.sourceKind !== "STRM_URL";
}

/**
 * Returns a copy of the current URL parameters with the new audio selection.
 * The first audio stream is the server-side default, so it is represented by an
 * absent parameter; every other stream is written explicitly.
 */
export function audioTrackSwitchParams(
  current: URLSearchParams,
  nextIndex: string | null,
  source: MediaSource | undefined,
): URLSearchParams {
  const next = new URLSearchParams(current);
  const streams = audioStreams(source);
  const firstIndex = streams.length > 0 ? String(streams[0].index) : null;
  if (nextIndex === null || nextIndex === firstIndex) {
    next.delete("audioStreamIndex");
  } else {
    next.set("audioStreamIndex", nextIndex);
  }
  return next;
}
