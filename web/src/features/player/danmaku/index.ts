/**
 * Public surface of the danmaku feature.
 *
 * `parse` is pure XML handling, `schedule` assigns lanes from the current playback window,
 * `engine` owns the requestAnimationFrame rendering, `settings`/`useDanmakuSettings` hold the
 * browser-local user preferences, and `load`/`useDanmakuTrack` handle fetching and Worker parsing.
 * `components/player-danmaku-overlay.tsx` wires them to the player.
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
  DANMAKU_AREA_FRACTIONS,
  DANMAKU_BASE_FONT_SIZE,
  DANMAKU_LANE_GAP,
  DANMAKU_LANE_HEIGHT,
  DANMAKU_LINE_HEIGHT_RATIO,
  DANMAKU_SEEK_THRESHOLD_SECONDS,
  FIXED_DURATION_SECONDS,
  SCROLL_DURATION_SECONDS,
  createDanmakuScheduler,
  isDanmakuEntryHidden,
  laneGeometry,
  matchesDanmakuKeyword,
  type DanmakuPlacement,
  type DanmakuScheduler,
  type DanmakuSchedulerOptions,
  type DanmakuViewport,
  type ScheduledDanmaku,
} from "./schedule";

export {
  DANMAKU_SETTINGS_LIMITS,
  DANMAKU_SETTINGS_STORAGE_KEY,
  DEFAULT_DANMAKU_LAYOUT,
  DEFAULT_DANMAKU_SETTINGS,
  danmakuLayoutFromSettings,
  danmakuLayoutsEqual,
  normalizeDanmakuKeyword,
  normalizeDanmakuKeywords,
  normalizeDanmakuSettings,
  readDanmakuSettings,
  saveDanmakuSettings,
  type DanmakuArea,
  type DanmakuLayoutOptions,
  type DanmakuSettings,
} from "./settings";

export {
  useDanmakuSettings,
  type DanmakuSettingsStore,
  type UseDanmakuSettingsOptions,
} from "./useDanmakuSettings";

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
