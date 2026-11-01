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
  | "unviable";

/** Banner sentence prepended to the degraded transcode copy. */
export const CLIENT_DECODE_FALLBACK_NOTICE: Record<ClientDecodeFallbackReason, string> = {
  stalled: "客户端解码长时间没有产出可播放画面，已切换为服务器转码。",
  slow: "客户端解码过慢，已切换为服务器转码。",
  unviable: "这个源无法在浏览器里实时客户端解码（10-bit 或超过 1080p 的软解），已切换为服务器转码。",
};

/**
 * Web Inspector diagnostic for a client decode fallback.
 *
 * `detail` must stay free of media URLs, query strings and tokens: it only carries a codec string or
 * a rejection reason code.
 */
export function clientDecodeFallbackDiagnostic(reason: ClientDecodeFallbackReason, detail?: string) {
  const suffix = detail ? `（${detail}）` : "";
  return `[Lux 播放器] 客户端解码回退为服务端转码：${reason}${suffix}`;
}
