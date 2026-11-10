/**
 * Why a client decode attempt handed playback back to the server.
 *
 * The runtime raises one of these when a client engine never produced a playable segment, when it
 * cannot keep up, or when the capability gate refuses the source up front. The player then shows the
 * same non-blocking degraded banner as any other server fallback, with one extra sentence that names
 * the reason, and logs a diagnostic for Web Inspector.
 */
export type ClientDecodeFallbackReason =
  /** No playable segment inside the first-segment window (Worker/MSE/demuxer never reported back). */
  | "stalled"
  /** Playback started but decoded throughput stayed clearly below realtime. */
  | "slow"
  /** The gate refused the source before the engine was even created (bit depth / 1080p WASM cap). */
  | "unviable"
  /** The native element advanced playback but never produced a video frame (`videoWidth === 0`). */
  | "silent-video"
  /** The native element claimed an audio track but decoded no audio frame at all. */
  | "silent-audio";

/** Banner sentence prepended to the degraded transcode copy. */
export const CLIENT_DECODE_FALLBACK_NOTICE: Record<ClientDecodeFallbackReason, string> = {
  stalled: "客户端解码长时间没有产出可播放画面，已切换为服务器转码。",
  slow: "客户端解码过慢，已切换为服务器转码。",
  unviable: "客户端解码无法播放这个源，已切换为服务器转码。",
  "silent-video": "浏览器只解出了声音、没有解出画面，已切换为服务器转码。",
  "silent-audio": "浏览器没有解出音轨，已切换为服务端音频转码。",
};

/**
 * Extra instructions for the retry that follows a failed Direct Play attempt.
 *
 * They only ever *narrow* what the browser claims about the source, so the server can pick a tier
 * that actually decodes the track the element failed on.
 */
export type PlaybackFallbackOptions = {
  /** The native element decoded no audio frame, so the retry must re-encode the audio track. */
  forceAudioTranscode?: boolean;
};

/**
 * Web Inspector diagnostic for a client decode fallback.
 *
 * `detail` must stay free of media URLs, query strings and tokens: it only carries a codec string or
 * a rejection reason code.
 */
export function clientDecodeFallbackDiagnostic(reason: ClientDecodeFallbackReason, detail?: string) {
  return `[Lux 播放器] 客户端解码回退为服务端转码：${reason}${diagnosticDetail(detail)}`;
}

/** Web Inspector diagnostic for a Direct Play failure that is being retried server-side. */
export function playbackFallbackDiagnostic(detail?: string) {
  return `[Lux 播放器] 直连播放失败，已回退为服务端转码${diagnosticDetail(detail)}`;
}

function diagnosticDetail(detail?: string) {
  return detail ? `（${detail}）` : "";
}
