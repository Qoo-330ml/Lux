const HLS_MIME = "application/vnd.apple.mpegurl";

/**
 * HLS.js uses Media Source Extensions when native HLS is unavailable. Keep
 * this capability check dependency-free so the large HLS.js runtime stays out
 * of the initial player chunk.
 */
export function canUseHls(video?: HTMLVideoElement | null) {
  if (video?.canPlayType(HLS_MIME) !== "") return true;
  return typeof MediaSource !== "undefined"
    && typeof MediaSource.isTypeSupported === "function"
    && MediaSource.isTypeSupported("video/mp4; codecs=\"avc1.42E01E,mp4a.40.2\"");
}
