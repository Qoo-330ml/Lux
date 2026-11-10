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
 * Who decides which audio stream the viewer actually hears.
 *
 * - `"browser"`: a Direct Play session hands the original container to the
 *   browser, which follows the container's own default track — not necessarily
 *   the first audio stream — so nothing is confirmed until the URL carries an
 *   explicit `audioStreamIndex`.
 * - `"server"`: a server-side session muxes exactly one audio stream, so the
 *   explicit index (when valid) or the first audio stream (`0:a:0`) is what plays.
 */
export type AudioTrackOwner = "browser" | "server";

/**
 * The index the player can prove is audible. Without an explicit
 * `audioStreamIndex` a server-side session maps the first audio stream (`0:a:0`),
 * not the stream flagged as default, so that first stream is the confirmation
 * fallback there; a Direct Play session confirms nothing on its own.
 */
export function resolveConfirmedAudioTrackIndex(
  source: MediaSource | undefined,
  requestedIndex: string | null | undefined,
  owner: AudioTrackOwner,
): string | null {
  const streams = audioStreams(source);
  if (streams.length === 0) return null;
  const requested = requestedIndex?.trim();
  if (requested && streams.some((stream) => String(stream.index) === requested)) {
    return requested;
  }
  return owner === "server" ? String(streams[0].index) : null;
}

export function audioTrackLabel(stream: MediaStream, position: number): string {
  const parts = [
    streamLabel(stream, "音频", position),
    audioChannelLabel(stream),
    stream.isDefault ? "默认" : undefined,
  ].filter((value): value is string => Boolean(value?.trim()));
  return parts.join(" · ");
}

/**
 * Lists the audio streams for the switcher. `confirmedIndex` must come from
 * `resolveConfirmedAudioTrackIndex`, so "（当前）" only marks a track the player can
 * prove: an explicit choice, or the server-side session's own mapping. An
 * unconfirmed Direct Play track stays unmarked.
 */
export function audioTrackOptions(
  source: MediaSource | undefined,
  confirmedIndex: string | null | undefined,
): AudioTrackOption[] {
  const selected = confirmedIndex?.trim() || null;
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
 *
 * A server-side session already maps the first audio stream when the parameter is
 * absent, so that choice is represented by an absent parameter and the URL stays
 * canonical. Direct Play cannot be steered that way: the browser follows the
 * container's own default track, so choosing the first audio stream has to be
 * written out explicitly. Without it the first track can never be selected, and
 * the request would not ask the server for the session that fixes the track.
 */
export function audioTrackSwitchParams(
  current: URLSearchParams,
  nextIndex: string | null,
  source: MediaSource | undefined,
  owner: AudioTrackOwner,
): URLSearchParams {
  const next = new URLSearchParams(current);
  const streams = audioStreams(source);
  const firstIndex = streams.length > 0 ? String(streams[0].index) : null;
  if (nextIndex === null || (owner === "server" && nextIndex === firstIndex)) {
    next.delete("audioStreamIndex");
  } else {
    next.set("audioStreamIndex", nextIndex);
  }
  return next;
}
