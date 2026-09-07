/**
 * Codec decision used by the optional Chrome extension media engine.
 *
 * This module deliberately contains no media side effects. A codec is only
 * considered native when the caller has already probed the complete MSE
 * combination. Software paths are explicit so an unsupported track can never
 * be silently dropped from a session.
 */
export type ChromeExtensionVideoPath = "native-mse" | "hevc-wasm" | "unsupported";
export type ChromeExtensionAudioPath = "native-mse" | "eac3-wasm" | "unsupported";

export type ChromeExtensionMediaCapability = {
  video: ChromeExtensionVideoPath;
  audio: ChromeExtensionAudioPath;
  videoCodec: string | null;
  audioCodec: string | null;
  requiresExtension: boolean;
  supported: boolean;
  reason?: string;
};

export type ChromeExtensionMediaProbe = {
  videoCodec?: string | null;
  audioCodec?: string | null;
  /** True only after probing the complete video+audio MSE codec string. */
  nativeMseSupported: boolean;
  /** True when the bundled HEVC decoder and H.264 output path are available. */
  hevcWasmAvailable?: boolean;
  /** True when the AC-3/E-AC-3 decoder and PCM output path are available. */
  eac3WasmAvailable?: boolean;
  /** True when the page can accept the H.264 output from the HEVC path. */
  h264OutputSupported?: boolean;
};

export function classifyChromeExtensionMedia(
  probe: ChromeExtensionMediaProbe,
): ChromeExtensionMediaCapability {
  const video = canonicalVideoCodec(probe.videoCodec);
  const audio = canonicalAudioCodec(probe.audioCodec);
  const hasVideo = Boolean(video);
  const audioInputProvided = Boolean(probe.audioCodec?.trim());
  const hasAudio = Boolean(audio);

  if (!hasVideo) {
    return unsupported(video, audio, "缺少或不支持的视频编码");
  }
  if (audioInputProvided && !hasAudio) {
    return unsupported(video, audio, "缺少或不支持的音频编码");
  }

  if (probe.nativeMseSupported) {
    return {
      video: "native-mse",
      audio: "native-mse",
      videoCodec: video,
      audioCodec: audio,
      requiresExtension: false,
      supported: true,
    };
  }

  const hevcFallback = video === "hevc"
    && probe.hevcWasmAvailable === true
    && probe.h264OutputSupported === true;
  const audioFallback = (audio === "ac-3" || audio === "ec-3")
    && probe.eac3WasmAvailable === true;

  if (hevcFallback && (!hasAudio || audio === "mp4a.40.2" || audio === "opus" || audioFallback)) {
    return {
      video: "hevc-wasm",
      audio: audioFallback ? "eac3-wasm" : "native-mse",
      videoCodec: video,
      audioCodec: audio,
      requiresExtension: true,
      supported: true,
    };
  }

  if (audioFallback && (video === "avc1" || video === "vp09" || video === "av01")) {
    return {
      video: "native-mse",
      audio: "eac3-wasm",
      videoCodec: video,
      audioCodec: audio,
      requiresExtension: true,
      supported: true,
    };
  }

  return unsupported(video, audio, "浏览器不支持该音视频组合，扩展也没有可用的窄解码器");
}

export function canonicalVideoCodec(codec: string | null | undefined): string | null {
  const normalized = codec?.trim().toLowerCase() ?? "";
  if (/^(?:h\.?264|avc|avc1|v_mpeg4\/iso\/avc)(?:\.|$)/u.test(normalized)) return "avc1";
  if (/^(?:hevc|h265|hvc1|hev1|v_mpegh\/iso\/hevc)(?:\.|$)/u.test(normalized)) return "hevc";
  if (/^(?:vp9|vp09|v_vp9)(?:\.|$)/u.test(normalized)) return "vp09";
  if (/^(?:av1|av01|v_av1)(?:\.|$)/u.test(normalized)) return "av01";
  return null;
}

export function canonicalAudioCodec(codec: string | null | undefined): string | null {
  const normalized = codec?.trim().toLowerCase() ?? "";
  if (/^(?:aac|a_aac|mp4a)(?:\.|$)/u.test(normalized)) return "mp4a.40.2";
  if (normalized === "opus" || normalized === "a_opus") return "opus";
  if (normalized === "ac3" || normalized === "ac-3" || normalized === "a_ac3") return "ac-3";
  if (normalized === "eac3" || normalized === "ec-3" || normalized === "a_eac3") return "ec-3";
  return null;
}

function unsupported(video: string | null, audio: string | null, reason: string): ChromeExtensionMediaCapability {
  return {
    video: "unsupported",
    audio: "unsupported",
    videoCodec: video,
    audioCodec: audio,
    requiresExtension: true,
    supported: false,
    reason,
  };
}
