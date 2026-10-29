import type { MediaItem, MediaSource } from "../../lib/api/types";
import type { PlayerEpisodeNavigation } from "./components/player-controls";
import type { PlaybackEngine } from "./playback-engine";

export const TICKS_PER_SECOND = 10_000_000;

export function getSubtitleInfo(media?: MediaItem | null) {
  if (!media) return null;
  if (media.itemType === "EPISODE") {
    const season = media.parentIndexNumber != null ? `第 ${media.parentIndexNumber} 季 ` : "";
    const episode = media.indexNumber != null ? `第 ${media.indexNumber} 集` : "";
    return `${season}${episode}`.trim() || null;
  }
  if (media.productionYear) {
    return String(media.productionYear);
  }
  return null;
}

export function screenshotFileName(title: string) {
  const normalized = title
    .replace(/[\\/:*?"<>|]/g, "-")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 80);
  return `${normalized || "lux-screenshot"}-${new Date().toISOString().replace(/[:.]/g, "-")}.png`;
}

export function timelinePosition(bar: HTMLDivElement, clientX: number, duration: number) {
  const rect = bar.getBoundingClientRect();
  if (rect.width <= 0 || duration <= 0) return 0;
  const progress = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
  return progress * duration;
}

export function episodeNavigationFor(episodes: MediaItem[], currentEpisodeId: string): PlayerEpisodeNavigation {
  const playableEpisodes = episodes.filter((episode) =>
    episode.itemType === "EPISODE" && (episode.mediaSources?.length ?? 0) > 0,
  );
  const currentIndex = playableEpisodes.findIndex((episode) => episode.id === currentEpisodeId);
  return {
    previousEpisodeId: currentIndex > 0 ? playableEpisodes[currentIndex - 1].id : null,
    nextEpisodeId: currentIndex >= 0 && currentIndex < playableEpisodes.length - 1
      ? playableEpisodes[currentIndex + 1].id
      : null,
  };
}

export function isMatroskaSource(source: MediaSource | undefined) {
  return (source?.container ?? "").toLowerCase().split(",").some((part) => part.trim() === "mkv" || part.trim() === "matroska" || part.trim() === "webm");
}

export function useClientEngine(engine: PlaybackEngine) {
  return engine.kind === "client-mkv" || engine.kind === "client-hevc";
}
