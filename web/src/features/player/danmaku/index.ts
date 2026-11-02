/**
 * Public surface of the danmaku feature.
 *
 * `parse` is pure XML handling, `schedule` assigns lanes from the current playback window,
 * `engine` owns the requestAnimationFrame rendering, and `load`/`useDanmakuTrack` handle fetching and
 * Worker parsing. `components/player-danmaku-overlay.tsx` wires them to the player.
 */

export {
  DANMAKU_LIMITS,
  DANMAKU_SERVER_MAX_BYTES,
  DanmakuParseError,
  decodeDanmakuBytes,
  exceedsUtf8ByteLimit,
  parseBilibiliDanmaku,
  parseDanmakuXml,
  type DanmakuMode,
  type DanmakuParseErrorCode,
  type DanmakuParseOptions,
  type DanmakuParseResult,
  type LuxDanmakuEntry,
} from "./parse";

export {
  DANMAKU_FONT_FAMILY,
  clearTextMeasurementCache,
  createTextMeasurer,
  estimateTextWidth,
  type MeasureText,
} from "./measure";

export {
  DANMAKU_LANE_GAP,
  DANMAKU_SEEK_THRESHOLD_SECONDS,
  FIXED_DURATION_SECONDS,
  FIXED_LINE_HEIGHT,
  SCROLL_DURATION_SECONDS,
  SCROLL_LINE_HEIGHT,
  createDanmakuScheduler,
  laneGeometry,
  type DanmakuPlacement,
  type DanmakuScheduler,
  type DanmakuSchedulerOptions,
  type DanmakuViewport,
  type ScheduledDanmaku,
} from "./schedule";

export {
  DANMAKU_MAX_INTERPOLATION_DRIFT_SECONDS,
  createDanmakuEngine,
  interpolateMediaTime,
  type DanmakuClockAnchor,
  type DanmakuClockState,
  type DanmakuEngine,
  type DanmakuEngineOptions,
} from "./engine";

export {
  danmakuFailureMessage,
  danmakuSourceKey,
  danmakuStatusMessage,
  forgetKnownEmptyDanmakuSources,
  isKnownEmptyDanmakuSource,
  loadDanmakuDocument,
  parseDanmakuPayload,
  readDanmakuResponse,
  rememberEmptyDanmakuSource,
  sameOriginRawUrl,
} from "./load";

export { useDanmakuTrack, type DanmakuTrack, type UseDanmakuTrackOptions } from "./useDanmakuTrack";
