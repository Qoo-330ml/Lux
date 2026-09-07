import type { CaptionFormat, LuxCaptionCue } from "./caption-parser";
import { MatroskaStreamDemuxer, parseMatroska, type MatroskaSample, type MatroskaTrack } from "./matroska-demuxer";
import { parseMatroskaCuesRange, locateMatroskaIndex, type MatroskaCue } from "./matroska-range-index";
import { MatroskaRangeReader } from "./matroska-range-reader";
import { parseMatroskaSubtitleSample } from "./matroska-subtitles";

const INITIAL_RANGE_BYTES = 1 * 1024 * 1024;
const MAX_CUES_RANGE_BYTES = 8 * 1024 * 1024;
const MAX_CLUSTER_RANGE_BYTES = 32 * 1024 * 1024;
const LOOKAHEAD_SECONDS = 45;

export type RemoteMkvCaptionSelection = {
  id: string;
  name?: string;
  language?: string;
  format?: CaptionFormat;
  ordinal?: number;
};

export type RemoteMkvCaptionReaderOptions = {
  source: string;
  selection: RemoteMkvCaptionSelection;
  currentTime: () => number;
  onCue: (cue: LuxCaptionCue & { trackId: string }) => void;
  onReady?: () => void;
  onError?: (error: Error) => void;
};

/**
 * Reads only Matroska metadata/Cue-selected clusters for text captions. The
 * native video element keeps the original media connection and decodes audio
 * and video; this reader never creates a MediaSource or touches audio/video
 * samples. Every request goes through the signed same-origin Range Relay.
 */
export class RemoteMkvCaptionReader {
  private readonly abortController = new AbortController();
  private readonly reader: MatroskaRangeReader;
  private readonly selection: RemoteMkvCaptionSelection;
  private readonly currentTime: () => number;
  private readonly onCue: RemoteMkvCaptionReaderOptions["onCue"];
  private readonly onReady?: () => void;
  private readonly onError?: (error: Error) => void;
  private readonly loadedClusters = new Set<number>();
  private requestedTime = 0;
  private windowPromise: Promise<void> | null = null;
  private demuxer: MatroskaStreamDemuxer | null = null;
  private cues: MatroskaCue[] = [];
  private timecodeScale = 1_000_000;
  private selectedTrack: MatroskaTrack | null = null;
  private indexReady = false;
  private stopped = false;

  constructor(options: RemoteMkvCaptionReaderOptions) {
    this.reader = new MatroskaRangeReader(options.source, {
      initialBytes: INITIAL_RANGE_BYTES,
      maxRangeBytes: MAX_CLUSTER_RANGE_BYTES,
    });
    this.selection = options.selection;
    this.currentTime = options.currentTime;
    this.onCue = options.onCue;
    this.onReady = options.onReady;
    this.onError = options.onError;
  }

  async start() {
    try {
      const first = await this.reader.readRange(0, INITIAL_RANGE_BYTES - 1, this.abortController.signal);
      const parsedHeader = parseMatroska(first.data);
      this.timecodeScale = parsedHeader.timecodeScale;
      this.selectedTrack = selectSubtitleTrack(parsedHeader.subtitleTracks, this.selection);
      if (!this.selectedTrack) throw new Error("未找到所选远程内嵌字幕轨道");
      const metadata = locateMatroskaIndex(first.data, first.range.total);
      const cuesEnd = Math.min(first.range.total - 1, metadata.cuesOffset + MAX_CUES_RANGE_BYTES - 1);
      const cuesRange = await this.reader.readRange(metadata.cuesOffset, cuesEnd, this.abortController.signal);
      const index = parseMatroskaCuesRange(
        cuesRange.data,
        cuesRange.range.start,
        cuesRange.range.total,
        metadata,
        parsedHeader.videoTrack?.number,
      );
      this.cues = index.cues;
      if (this.cues.length === 0) throw new Error("远程媒体没有可定位的字幕时间索引");
      this.demuxer = new MatroskaStreamDemuxer({
        onSample: (sample) => this.handleSample(sample),
        onError: (error) => this.fail(error instanceof Error ? error : new Error(String(error))),
      }, { tracks: [...parsedHeader.subtitleTracks, ...(parsedHeader.videoTrack ? [parsedHeader.videoTrack] : []), ...(parsedHeader.audioTrack ? [parsedHeader.audioTrack] : [])], timecodeScale: this.timecodeScale });
      this.indexReady = true;
      this.onReady?.();
      this.setTime(this.currentTime());
    } catch (error) {
      if (!this.abortController.signal.aborted) this.fail(error instanceof Error ? error : new Error(String(error)));
    }
  }

  setTime(time: number) {
    if (this.stopped || !this.indexReady || !Number.isFinite(time)) return;
    this.requestedTime = Math.max(0, time);
    if (!this.windowPromise) {
      this.windowPromise = this.loadWindows().finally(() => {
        this.windowPromise = null;
        if (!this.stopped && this.requestedTime !== time) this.setTime(this.requestedTime);
      });
    }
  }

  destroy() {
    this.stopped = true;
    this.abortController.abort();
    this.demuxer = null;
    this.cues = [];
    this.loadedClusters.clear();
  }

  private async loadWindows() {
    while (!this.stopped) {
      const target = this.requestedTime;
      await this.loadWindow(target);
      if (target === this.requestedTime) return;
    }
  }

  private async loadWindow(time: number) {
    const scale = this.timecodeScale / 1_000_000;
    const startTick = Math.max(0, Math.floor((time - 2) * 1000 / scale));
    const endTick = Math.ceil((time + LOOKAHEAD_SECONDS) * 1000 / scale);
    let startIndex = 0;
    for (let index = 0; index < this.cues.length; index += 1) {
      if (this.cues[index].timecode <= startTick) startIndex = index;
      else break;
    }
    const clusterOffsets: number[] = [];
    for (let index = startIndex; index < this.cues.length && this.cues[index].timecode <= endTick; index += 1) {
      const offset = this.cues[index].clusterOffset;
      if (!clusterOffsets.includes(offset)) clusterOffsets.push(offset);
    }
    for (const offset of clusterOffsets) {
      if (this.stopped || this.loadedClusters.has(offset)) continue;
      await this.loadCluster(offset);
    }
  }

  private async loadCluster(clusterOffset: number) {
    const nextOffset = this.cues.find((cue) => cue.clusterOffset > clusterOffset)?.clusterOffset;
    const end = Math.min(
      (this.reader.total ?? Number.MAX_SAFE_INTEGER) - 1,
      nextOffset !== undefined ? nextOffset - 1 : clusterOffset + MAX_CLUSTER_RANGE_BYTES - 1,
    );
    if (end < clusterOffset) return;
    const result = await this.reader.readRange(clusterOffset, end, this.abortController.signal);
    if (this.stopped || !this.demuxer) return;
    this.loadedClusters.add(clusterOffset);
    this.demuxer.write(result.data);
  }

  private handleSample(sample: MatroskaSample) {
    if (!this.selectedTrack || sample.trackNumber !== this.selectedTrack.number) return;
    try {
      const cue = parseMatroskaSubtitleSample(
        sample.data,
        this.selectedTrack,
        sample.timestampMs,
        sample.durationMs,
      );
      if (cue) this.onCue({ ...cue, trackId: this.selection.id });
    } catch (error) {
      this.fail(error instanceof Error ? error : new Error(String(error)));
    }
  }

  private fail(error: Error) {
    if (this.stopped) return;
    this.stopped = true;
    this.abortController.abort();
    this.onError?.(error);
  }
}

function selectSubtitleTrack(tracks: readonly MatroskaTrack[], selection: RemoteMkvCaptionSelection) {
  const supported = tracks.filter((track) => isSupportedSubtitleCodec(track.codecId));
  if (supported.length === 0) return null;
  const expectedName = normalize(selection.name);
  const expectedLanguage = normalize(selection.language);
  const expectedFormat = selection.format === "srt" ? "S_TEXT/UTF8" : selection.format ? `S_TEXT/${selection.format.toUpperCase()}` : null;
  const scored = supported.map((track, ordinal) => ({
    track,
    ordinal,
    score: (expectedName && normalize(track.name) === expectedName ? 8 : 0)
      + (expectedLanguage && (normalize(track.languageBcp47) === expectedLanguage || normalize(track.language) === expectedLanguage) ? 4 : 0)
      + (expectedFormat && track.codecId.toUpperCase() === expectedFormat ? 2 : 0)
      + (selection.ordinal === ordinal ? 1 : 0),
  }));
  return scored.sort((left, right) => right.score - left.score || left.ordinal - right.ordinal)[0]?.track ?? null;
}

function isSupportedSubtitleCodec(codec: string) {
  const normalized = codec.trim().toUpperCase();
  return normalized === "S_TEXT/UTF8" || normalized === "S_TEXT/ASS" || normalized === "S_TEXT/SSA";
}

function normalize(value: string | null | undefined) {
  return value?.trim().toLowerCase() || null;
}
