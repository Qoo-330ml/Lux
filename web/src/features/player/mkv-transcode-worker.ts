import { FMP4Muxer, H264Encoder, HEVCDecoder, type HEVCFrame, type MuxerSample } from "@hevcjs/core";
import { Box, createFile, DataStream, ISOFile } from "mp4box";
import processPolyfill from "process";
import { MatroskaStreamDemuxer, type MatroskaSample, type MatroskaTrack } from "./matroska-demuxer";
import { addHevcTrack, concatBuffers, hevcCodecString, makeAacEsdsData, matroskaTimestampTicks, toLengthPrefixed } from "./mkv-remux";
import { encodedVideoDurationTicks, isSupportedMatroskaAudio, isSupportedMatroskaVideo, matroskaAudioConfig, toAnnexB } from "./mkv-transcode";

type WorkerMessage =
  | { type: "init"; wasmUrl: string; wasmBinaryUrl: string; mode?: "sdr" | "hevc-remux" }
  | { type: "data"; data: ArrayBuffer }
  | { type: "flush" }
  | { type: "destroy" };

type WorkerResponse =
  | { type: "ready" }
  | { type: "init"; initSegment: ArrayBuffer; codec: string }
  | { type: "segment"; mediaSegment: ArrayBuffer; mediaDurationMs: number; processingDurationMs: number }
  | { type: "done" }
  | { type: "error"; message: string };

type AudioMuxerSample = { data: Uint8Array; duration: number };
type TimestampedAudioSample = AudioMuxerSample & { timestampMs: number };
type RemuxFile = ReturnType<typeof createFile>;

let decoder: HEVCDecoder | null = null;
let encoder: H264Encoder | null = null;
let muxer: FMP4Muxer | null = null;
let demuxer: MatroskaStreamDemuxer | null = null;
let videoTrack: MatroskaTrack | null = null;
let audioTrack: MatroskaTrack | null = null;
let audioConfig: ReturnType<typeof matroskaAudioConfig> = null;
let pendingVideo: MuxerSample[] = [];
let pendingAudio: TimestampedAudioSample[] = [];
let pendingVideoStartMs: number | null = null;
let pendingVideoEndMs = 0;
let mediaDurationMs = 0;
let processingDurationMs = 0;
let chain = Promise.resolve();
let sampleChain = Promise.resolve();
let fatalError: Error | null = null;
let initialized = false;
let remuxMode = false;
let remuxFile: RemuxFile | null = null;
let remuxVideoTrackId: number | null = null;
let remuxAudioTrackId: number | null = null;
let remuxVideoSampleCount = 0;
let remuxAudioSampleCount = 0;
let remuxBatchStartMs: number | null = null;
let remuxBatchEndMs = 0;
let remuxOriginMs: number | null = null;
let remuxSequenceNumber = 0;

const workerScope = globalThis as typeof globalThis & {
  onmessage: ((event: MessageEvent<WorkerMessage>) => void) | null;
  postMessage(message: WorkerResponse, transfer?: Transferable[]): void;
};

workerScope.onmessage = (event) => {
  if (event.data.type === "init") {
    chain = chain.then(() => initialize(event.data));
  } else if (event.data.type === "data") {
    chain = chain.then(() => consumeData(event.data.data));
  } else if (event.data.type === "flush") {
    chain = chain.then(flush);
  } else if (event.data.type === "destroy") {
    destroy();
  }
  chain.catch((error) => reportError(error));
};

async function initialize(message: Extract<WorkerMessage, { type: "init" }>) {
  destroy();
  (globalThis as typeof globalThis & { process?: unknown }).process ??= processPolyfill;
  remuxMode = message.mode === "hevc-remux";
  if (!remuxMode) {
    decoder = await HEVCDecoder.create({ wasmUrl: message.wasmUrl, wasmBinaryUrl: message.wasmBinaryUrl });
    muxer = new FMP4Muxer();
  }
  demuxer = new MatroskaStreamDemuxer({
    onTrack: (track) => {
      if (track.type === "video" && !videoTrack) {
        if (!isSupportedMatroskaVideo(track)) throw new Error(`MKV 视频编码不支持：${track.codecId}`);
        videoTrack = track;
      } else if (track.type === "audio" && !audioTrack) {
        audioTrack = track;
        audioConfig = matroskaAudioConfig(track);
        if (track.codecId.toUpperCase().startsWith("A_AAC") && !audioConfig) throw new Error("MKV 音频只支持 AAC-LC");
      }
    },
    onSample: (sample) => {
      sampleChain = sampleChain.then(() => consumeSample(sample));
      sampleChain.catch((error) => reportError(error));
    },
    onError: reportError,
  });
  initialized = true;
  workerScope.postMessage({ type: "ready" });
}

async function consumeData(data: ArrayBuffer) {
  if (!initialized || fatalError || !demuxer) return;
  demuxer.write(new Uint8Array(data));
}

async function consumeSample(sample: MatroskaSample) {
  if (remuxMode) {
    await consumeRemuxSample(sample);
    return;
  }
  if (fatalError || !decoder) return;
  const track = sample.trackNumber === videoTrack?.number ? videoTrack : sample.trackNumber === audioTrack?.number ? audioTrack : null;
  if (!track) return;
  if (track.type === "audio") {
    if (!audioConfig || !audioTrack?.sampleRate) return;
    pendingAudio.push({
      timestampMs: sample.timestampMs,
      data: sample.data,
      duration: Math.max(1, Math.round(sample.durationMs * audioTrack.sampleRate / 1000)),
    });
    await flushIfReady(sample.timestampMs);
    return;
  }
  const startedAt = performance.now();
  decoder.feed(toAnnexB(sample.data));
  const frames = decoder.drain();
  if (frames.length === 0) return;
  await encodeFrames(frames, sample.timestampMs, sample.durationMs, sample.keyframe);
  processingDurationMs += performance.now() - startedAt;
  mediaDurationMs += sample.durationMs;
  await flushIfReady(sample.timestampMs);
}

async function encodeFrames(frames: HEVCFrame[], timestampMs: number, durationMs: number, keyframe: boolean) {
  if (!encoder) {
    const first = frames[0];
    encoder = new H264Encoder({
      width: first.width,
      height: first.height,
      fps: durationMs > 0 ? 1000 / durationMs : 25,
      bitrate: first.width * first.height * 4,
    });
  }
  const encoded: MuxerSample[] = [];
  encoder.onChunk = (chunk) => encoded.push({
    data: chunk.data,
    duration: encodedVideoDurationTicks(chunk.duration, durationMs),
    isKeyframe: chunk.isKeyframe,
    compositionTimeOffset: 0,
  });
  const frameDuration = frames.length > 0 ? durationMs / frames.length : durationMs;
  frames.forEach((frame, index) => encoder?.encode(frame, Math.round((timestampMs + index * frameDuration) * 1000), keyframe && index === 0));
  await encoder.flush();
  pendingVideo.push(...encoded);
  if (pendingVideoStartMs === null) pendingVideoStartMs = timestampMs;
  pendingVideoEndMs = Math.max(pendingVideoEndMs, timestampMs + durationMs);
  if (!workerScopeHasInit()) emitInit(frames[0]);
}

function workerScopeHasInit() {
  return Boolean((workerScope as typeof workerScope & { __mkvInitSent?: boolean }).__mkvInitSent);
}

function emitInit(frame: HEVCFrame) {
  if (!encoder?.codecDescription || !muxer) return;
  const video = { width: frame.width, height: frame.height, timescale: 90_000, avcC: encoder.codecDescription };
  const initSegment = audioConfig && audioTrack?.sampleRate && audioTrack.channels
    ? muxer.generateInitAV(video, { timescale: audioTrack.sampleRate, channelCount: audioTrack.channels, sampleRate: audioTrack.sampleRate, sampleSize: 16, asc: audioConfig.asc })
    : muxer.generateInit(video);
  const codec = audioConfig ? `${encoder.codec},mp4a.40.2` : encoder.codec;
  (workerScope as typeof workerScope & { __mkvInitSent?: boolean }).__mkvInitSent = true;
  const transfer = copyTransferBuffer(initSegment);
  workerScope.postMessage({ type: "init", initSegment: transfer, codec }, [transfer]);
}

async function flushIfReady(timestampMs: number) {
  if (pendingVideoStartMs !== null && timestampMs - pendingVideoStartMs >= 2_000) await flushSegment();
}

async function flushSegment() {
  if (!muxer || pendingVideo.length === 0 || pendingVideoStartMs === null) return;
  const endMs = pendingVideoEndMs;
  const audio = pendingAudio.filter((sample) => sample.timestampMs < endMs + 100);
  pendingAudio = pendingAudio.slice(audio.length);
  const videoBaseTime = Math.max(0, Math.round(pendingVideoStartMs * 90));
  const audioBaseTime = audio.length > 0 && audioTrack?.sampleRate ? Math.max(0, Math.round(audio[0].timestampMs * audioTrack.sampleRate / 1000)) : 0;
  const mediaSegment = audioConfig && audio.length > 0
    ? muxer.muxSegmentAV(pendingVideo, videoBaseTime, audio, audioBaseTime)
    : muxer.muxSegment(pendingVideo, videoBaseTime);
  const duration = Math.max(0, endMs - pendingVideoStartMs);
  const transfer = copyTransferBuffer(mediaSegment);
  workerScope.postMessage({ type: "segment", mediaSegment: transfer, mediaDurationMs: duration, processingDurationMs }, [transfer]);
  pendingVideo = [];
  pendingVideoStartMs = null;
  pendingVideoEndMs = 0;
}

async function consumeRemuxSample(sample: MatroskaSample) {
  if (fatalError) return;
  const track = sample.trackNumber === videoTrack?.number ? videoTrack : sample.trackNumber === audioTrack?.number ? audioTrack : null;
  if (!track) return;
  const file = ensureRemuxFile();
  const timestampMs = Math.max(0, sample.timestampMs - (remuxOriginMs ?? sample.timestampMs));
  const durationMs = sample.durationMs > 0
    ? sample.durationMs
    : track.type === "audio" && track.sampleRate
      ? 1024_000 / track.sampleRate
      : videoTrack?.defaultDurationMs ?? 1000 / 30;
  const isVideo = track.type === "video";
  const trackId = isVideo ? remuxVideoTrackId : remuxAudioTrackId;
  if (!trackId) return;
  const timescale = isVideo ? 90_000 : track.sampleRate ?? 48_000;
  const data = isVideo ? toLengthPrefixed(sample.data) : sample.data;
  const added = file.addSample(trackId, data.slice(), {
    duration: Math.max(1, matroskaTimestampTicks(durationMs, timescale)),
    dts: matroskaTimestampTicks(timestampMs, timescale),
    cts: matroskaTimestampTicks(timestampMs, timescale),
    is_sync: isVideo ? sample.keyframe : true,
  });
  if (!added) throw new Error("MKV fMP4 remux 添加样本失败");
  if (isVideo) remuxVideoSampleCount += 1;
  else remuxAudioSampleCount += 1;
  if (remuxBatchStartMs === null) remuxBatchStartMs = timestampMs;
  remuxBatchEndMs = Math.max(remuxBatchEndMs, timestampMs + durationMs);
  if (remuxBatchEndMs - remuxBatchStartMs >= 2_000) await flushRemuxSegment();
}

function ensureRemuxFile() {
  if (remuxFile) return remuxFile;
  if (!videoTrack?.width || !videoTrack.height || videoTrack.codecPrivate.byteLength < 23) {
    throw new Error("MKV HEVC 缺少有效的视频配置");
  }
  if (audioTrack && !audioConfig) throw new Error("MKV 音频只支持 AAC-LC");
  remuxOriginMs ??= 0;
  const file = createFile();
  const videoTrackId = addHevcTrack(file, videoTrack.codecPrivate, videoTrack.width, videoTrack.height);
  let audioTrackId: number | null = null;
  if (audioTrack && audioConfig) {
    const sampleRate = audioTrack.sampleRate;
    const channels = audioTrack.channels;
    if (!sampleRate || !channels) throw new Error("MKV AAC 音频缺少采样率或声道数");
    audioTrackId = file.addTrack({
      type: "mp4a",
      timescale: sampleRate,
      channel_count: channels,
      samplerate: sampleRate,
      samplesize: 16,
      hdlr: "soun",
    });
    if (!audioTrackId) throw new Error("MKV fMP4 remux 创建音频轨失败");
    const track = file.moov.traks.find((entry) => entry.tkhd.track_id === audioTrackId);
    const entry = track?.mdia.minf.stbl.stsd.entries[0];
    if (!entry) throw new Error("MKV fMP4 remux 缺少音频样本描述");
    const esds = new Box();
    esds.type = "esds";
    esds.data = makeAacEsdsData(audioConfig.asc);
    entry.addBox(esds);
  }
  removeCreatedMovieExtends(file);
  file.nextMoofNumber = remuxSequenceNumber;
  remuxFile = file;
  remuxVideoTrackId = videoTrackId;
  remuxAudioTrackId = audioTrackId;
  if (!workerScopeHasInit()) {
    const initSegment = ISOFile.writeInitializationSegment(file.ftyp, file.moov, 0, audioTrackId ? new Set([audioTrackId]) : undefined);
    const transfer = copyTransferBuffer(new Uint8Array(initSegment));
    const codec = audioTrackId ? `${hevcCodecString(videoTrack.codecPrivate)},mp4a.40.2` : hevcCodecString(videoTrack.codecPrivate);
    (workerScope as typeof workerScope & { __mkvInitSent?: boolean }).__mkvInitSent = true;
    workerScope.postMessage({ type: "init", initSegment: transfer, codec }, [transfer]);
  }
  return file;
}

async function flushRemuxSegment() {
  if (!remuxFile || remuxBatchStartMs === null || (remuxVideoSampleCount === 0 && remuxAudioSampleCount === 0)) return;
  const fragments: ArrayBuffer[] = [];
  remuxFile.nextMoofNumber = remuxSequenceNumber;
  if (remuxVideoTrackId && remuxVideoSampleCount > 0) {
    fragments.push(copyDataStream(remuxFile.createFragment(remuxVideoTrackId, 0, remuxVideoSampleCount - 1, new DataStream())));
  }
  if (remuxAudioTrackId && remuxAudioSampleCount > 0) {
    fragments.push(copyDataStream(remuxFile.createFragment(remuxAudioTrackId, 0, remuxAudioSampleCount - 1, new DataStream())));
  }
  remuxSequenceNumber = remuxFile.nextMoofNumber;
  const mediaSegment = concatBuffers(fragments);
  const transfer = copyTransferBuffer(new Uint8Array(mediaSegment));
  workerScope.postMessage({ type: "segment", mediaSegment: transfer, mediaDurationMs: remuxBatchEndMs - remuxBatchStartMs, processingDurationMs: 0 }, [transfer]);
  remuxFile = null;
  remuxVideoSampleCount = 0;
  remuxAudioSampleCount = 0;
  remuxBatchStartMs = null;
  remuxBatchEndMs = 0;
}

function removeCreatedMovieExtends(file: RemuxFile) {
  const movie = file.moov as typeof file.moov & { boxes?: Array<{ type: string }>; mvex?: unknown };
  const movieExtends = movie.mvex;
  if (!movieExtends || !movie.boxes) return;
  movie.boxes = movie.boxes.filter((box) => box !== movieExtends);
  delete movie.mvex;
}

function copyDataStream(stream: DataStream | undefined) {
  if (!stream) throw new Error("MKV fMP4 remux 生成空媒体片段");
  return new Uint8Array(stream.buffer, stream.byteOffset, stream.byteLength).slice().buffer;
}

async function flush() {
  if (fatalError) return;
  demuxer?.end();
  await sampleChain;
  if (remuxMode) {
    await flushRemuxSegment();
    workerScope.postMessage({ type: "done" });
    return;
  }
  if (!decoder) return;
  const frames = decoder.flush();
  if (frames.length > 0) await encodeFrames(frames, pendingVideoEndMs, videoTrack?.defaultDurationMs ?? 0, false);
  await encoder?.flush();
  await flushSegment();
  workerScope.postMessage({ type: "done" });
}

function destroy() {
  demuxer = null;
  decoder?.destroy();
  encoder?.close();
  decoder = null;
  encoder = null;
  muxer = null;
  videoTrack = null;
  audioTrack = null;
  audioConfig = null;
  pendingVideo = [];
  pendingAudio = [];
  pendingVideoStartMs = null;
  pendingVideoEndMs = 0;
  mediaDurationMs = 0;
  processingDurationMs = 0;
  fatalError = null;
  initialized = false;
  remuxMode = false;
  remuxFile = null;
  remuxVideoTrackId = null;
  remuxAudioTrackId = null;
  remuxVideoSampleCount = 0;
  remuxAudioSampleCount = 0;
  remuxBatchStartMs = null;
  remuxBatchEndMs = 0;
  remuxOriginMs = null;
  remuxSequenceNumber = 0;
  sampleChain = Promise.resolve();
  (workerScope as typeof workerScope & { __mkvInitSent?: boolean }).__mkvInitSent = false;
}

function reportError(error: unknown) {
  if (fatalError) return;
  fatalError = error instanceof Error ? error : new Error(String(error));
  workerScope.postMessage({ type: "error", message: fatalError.message });
}

function copyTransferBuffer(data: Uint8Array): ArrayBuffer {
  const copy = new Uint8Array(data.byteLength);
  copy.set(data);
  return copy.buffer;
}
