import { episodeTitle, mediaTitle, playbackProgress } from "../home/media-display";
import type { MediaItem } from "../../lib/api/types";

const dayMilliseconds = 86_400_000;

function pad(value: number) {
  return String(value).padStart(2, "0");
}

function startOfLocalDay(value: Date) {
  return Date.UTC(value.getFullYear(), value.getMonth(), value.getDate());
}

/**
 * When the user last watched an entry, in the words a history list reads best: the clock time for
 * today and yesterday, the day count inside the last week, and an explicit date afterwards.
 * Unknown or nonsensical timestamps render nothing rather than a fake date.
 */
export function watchHistoryTimeLabel(lastPlayedAt?: number | null, now = new Date()): string | undefined {
  if (lastPlayedAt == null || !Number.isSafeInteger(lastPlayedAt) || lastPlayedAt <= 0) return undefined;
  const playedAt = new Date(lastPlayedAt * 1000);
  const playedTime = playedAt.getTime();
  if (!Number.isFinite(playedTime)) return undefined;
  const clock = `${pad(playedAt.getHours())}:${pad(playedAt.getMinutes())}`;
  const elapsedDays = Math.round((startOfLocalDay(now) - startOfLocalDay(playedAt)) / dayMilliseconds);
  if (elapsedDays === 0) return `今天 ${clock}`;
  if (elapsedDays === 1) return `昨天 ${clock}`;
  if (elapsedDays > 1 && elapsedDays < 7) return `${elapsedDays} 天前`;
  const year = playedAt.getFullYear() === now.getFullYear() ? "" : `${playedAt.getFullYear()}-`;
  return `${year}${pad(playedAt.getMonth() + 1)}-${pad(playedAt.getDate())} ${clock}`;
}

/** What a history row says about watching progress: finished, a percentage, or nothing yet. */
export function watchHistoryProgressLabel(item: MediaItem): string {
  if (item.userData?.isPlayed) return "已看完";
  const progress = playbackProgress(item);
  if (progress <= 0) return "";
  return progress >= 100 ? "已看完" : `已看 ${progress}%`;
}

/** Row title: an episode stays grouped under its series, a movie uses its own title. */
export function watchHistoryTitle(item: MediaItem): string {
  if (item.itemType === "EPISODE" && item.seriesName) return item.seriesName;
  return mediaTitle(item);
}

/** Row subtitle: the season/episode line for episodes, the kind and year for everything else. */
export function watchHistorySubtitle(item: MediaItem): string {
  if (item.itemType === "EPISODE") return episodeTitle(item).replace(" · ", "·");
  return item.productionYear != null ? String(item.productionYear) : "";
}
