import { useCallback, useEffect, useRef } from "react";
import type { MediaSource, PlaybackState, WebPlaybackPlan } from "../../lib/api/types";
import {
  NativeVideoEngine,
  PLAYBACK_PERFORMANCE_EVENT,
  type PlaybackEngine,
  type PlaybackPerformance,
} from "./playback-engine";
import {
  DIRECT_CANDIDATE_TIMEOUT_MS,
  rememberCandidate,
  type DirectCandidate,
} from "./direct-candidates";
import { shouldUseClientHevc, shouldUseClientMkv } from "./playback-selection";
import { LegacyPlaybackEngineAdapter } from "./core/legacy-engine-adapter";
import { LuxPlayerRuntime } from "./core/player-runtime";
import { classifyPlayerEngineFailure } from "./components/player-diagnostics";
import { createPlaybackTimelineScheduler } from "./player-timeline-scheduler";
import { TICKS_PER_SECOND, isMatroskaSource, useClientEngine } from "./player-page-helpers";
import type { PlaybackReporting } from "./usePlaybackReporting";

export const TIMELINE_UI_UPDATE_INTERVAL_MS = 100;

const HEVC_RUNTIME_ASSETS = {
  workerUrl: "/hevc/transcode-worker.js",
  wasmUrl: "/hevc/hevc-decode.js",
  wasmModuleUrl: "/hevc/hevc-decode-module.js",
  wasmBinaryUrl: "/hevc/hevc-decode.wasm",
};

export type PlaybackEngineHandoff = {
  playbackKey: string;
  sourceId: string;
  currentTime: number;
  playing: boolean;
};

type Ref<T> = { current: T };

type PlaybackRuntimeOptions = {
  playbackKey: string;
  source: MediaSource | undefined;
  playbackPlan: WebPlaybackPlan | undefined;
  streamUrl: string;
  poster: string | null | undefined;
  candidate: DirectCandidate | undefined;
  candidates: readonly DirectCandidate[];
  hasNextCandidate: boolean;
  remoteHttpSource: boolean;
  clientMkvSourceUrl: string | null;
  playbackDataRef: Ref<PlaybackState | undefined>;
  hasRestoredPositionRef: Ref<boolean>;
  videoRef: Ref<HTMLVideoElement | null>;
  engineRef: Ref<PlaybackEngine | null>;
  runtimeRef: Ref<LuxPlayerRuntime | null>;
  currentTimeRef: Ref<number>;
  durationRef: Ref<number>;
  bufferedEndRef: Ref<number>;
  isDraggingScrubberRef: Ref<boolean>;
  playbackSessionIdRef: Ref<string | null>;
  reportPlayback: PlaybackReporting["reportPlayback"];
  stopActiveSession: PlaybackReporting["stopActiveSession"];
  markStarted: () => void;
  requestServerFallback: (reason?: unknown) => Promise<void>;
  setCurrentTime: (value: number) => void;
  setDuration: (value: number) => void;
  setBufferedEnd: (value: number) => void;
  setPlaying: (value: boolean) => void;
  setControlsVisible: (value: boolean) => void;
  setControlActivity: (updater: (activity: number) => number) => void;
  setFailedStreamUrl: (value: string | null) => void;
  setPlaybackFailure: (value: ReturnType<typeof classifyPlayerEngineFailure> | null) => void;
  setFallbackLoading: (value: boolean) => void;
  setFallbackSpeedX: (value: number | null) => void;
  setClientEngineActive: (value: boolean) => void;
};

/**
 * Loads the stream into the playback runtime and mirrors runtime events into page
 * state. The resume position, engine handoff across source/candidate switches and
 * the candidate metadata timeout are all part of this one lifecycle, so they move
 * together from PlayerPage.
 */
export function usePlaybackRuntime({
  playbackKey,
  source,
  playbackPlan,
  streamUrl,
  poster,
  candidate,
  candidates,
  hasNextCandidate,
  remoteHttpSource,
  clientMkvSourceUrl,
  playbackDataRef,
  hasRestoredPositionRef,
  videoRef,
  engineRef,
  runtimeRef,
  currentTimeRef,
  durationRef,
  bufferedEndRef,
  isDraggingScrubberRef,
  playbackSessionIdRef,
  reportPlayback,
  stopActiveSession,
  markStarted,
  requestServerFallback,
  setCurrentTime,
  setDuration,
  setBufferedEnd,
  setPlaying,
  setControlsVisible,
  setControlActivity,
  setFailedStreamUrl,
  setPlaybackFailure,
  setFallbackLoading,
  setFallbackSpeedX,
  setClientEngineActive,
}: PlaybackRuntimeOptions) {
  const playbackEngineHandoffRef = useRef<PlaybackEngineHandoff | null>(null);

  const restorePlaybackPosition = useCallback(() => {
    if (hasRestoredPositionRef.current) return;
    const video = videoRef.current;
    const playbackData = playbackDataRef.current;
    if (!video || !playbackData) return;
    if (video.readyState < 1 && !Number.isFinite(video.duration)) return;
    hasRestoredPositionRef.current = true;
    const resumeTicks = playbackData.positionTicks ?? 0;
    if (playbackData.isPlayed || resumeTicks <= 0) return;
    const resumeSeconds = resumeTicks / TICKS_PER_SECOND;
    if (!Number.isFinite(video.duration) || resumeSeconds < video.duration) {
      video.currentTime = resumeSeconds;
    }
  }, [hasRestoredPositionRef, playbackDataRef, videoRef]);

  useEffect(() => {
    restorePlaybackPosition();
  }, [restorePlaybackPosition]);

  useEffect(() => {
    const initialEngine = engineRef.current
      ?? (videoRef.current ? new NativeVideoEngine(videoRef.current) : null);
    if (!initialEngine || !streamUrl) return;
    const runtime = runtimeRef.current ?? new LuxPlayerRuntime();
    runtimeRef.current = runtime;
    engineRef.current = initialEngine;
    let activeEngine: PlaybackEngine = initialEngine;
    let performanceElement: HTMLVideoElement | null = null;
    let cancelled = false;
    const timelineScheduler = createPlaybackTimelineScheduler(
      (snapshot) => {
        if (!isDraggingScrubberRef.current) setCurrentTime(snapshot.currentTime);
        setDuration(snapshot.duration);
        setBufferedEnd(snapshot.bufferedEnd);
      },
      undefined,
      undefined,
      { minIntervalMs: TIMELINE_UI_UPDATE_INTERVAL_MS },
    );
    const syncSnapshot = (snapshot: {
      currentTime: number;
      duration: number | null;
      bufferedEnd?: number;
    }, immediate = false) => {
      const ranges = activeEngine.element.buffered;
      const next = {
        currentTime: snapshot.currentTime,
        duration: snapshot.duration ?? 0,
        bufferedEnd: snapshot.bufferedEnd
          ?? (ranges.length > 0 ? ranges.end(ranges.length - 1) : snapshot.currentTime),
      };
      currentTimeRef.current = next.currentTime;
      durationRef.current = next.duration;
      bufferedEndRef.current = next.bufferedEnd;
      timelineScheduler.schedule(next, immediate);
    };
    const restoreEngineHandoff = async () => {
      const handoff = playbackEngineHandoffRef.current;
      if (
        !handoff
        || handoff.playbackKey !== playbackKey
        || handoff.sourceId !== (source?.id ?? "")
      ) {
        return;
      }
      playbackEngineHandoffRef.current = null;
      if (Number.isFinite(handoff.currentTime) && handoff.currentTime >= 0) {
        currentTimeRef.current = handoff.currentTime;
        setCurrentTime(handoff.currentTime);
        activeEngine.seek(handoff.currentTime);
      }
      if (handoff.playing) {
        await activeEngine.play().catch(() => undefined);
      } else {
        activeEngine.pause();
      }
    };
    const removeRuntimeSubscription = runtime.subscribeEvents((event) => {
      if (cancelled) return;
      switch (event.type) {
        case "SOURCE_READY":
          syncSnapshot(event.snapshot, true);
          restorePlaybackPosition();
          void restoreEngineHandoff();
          break;
        case "PLAYING":
          markStarted();
          setPlaying(true);
          setControlsVisible(true);
          setControlActivity((activity) => activity + 1);
          void reportPlayback("PLAYING", true, false, initialEngine.element);
          break;
        case "PAUSED":
          if (event.snapshot) syncSnapshot(event.snapshot, true);
          setPlaying(false);
          setControlsVisible(true);
          if (!event.snapshot?.ended) {
            void reportPlayback("PAUSED", true, false, initialEngine.element);
          }
          break;
        case "WAITING":
          setControlsVisible(true);
          break;
        case "SEEK_START":
          currentTimeRef.current = event.position;
          setCurrentTime(event.position);
          break;
        case "SEEKED":
          syncSnapshot(event.snapshot, true);
          break;
        case "TIME_UPDATE":
          syncSnapshot(event.snapshot);
          void reportPlayback("PLAYING", false, false, initialEngine.element);
          break;
        case "ENDED": {
          syncSnapshot(event.snapshot, true);
          if (initialEngine.element.loop) {
            runtime.seek(0);
            currentTimeRef.current = 0;
            setCurrentTime(0);
            void runtime.play().catch((cause) => {
              if (!cancelled) {
                setFailedStreamUrl(streamUrl);
                setPlaybackFailure(classifyPlayerEngineFailure(cause));
              }
            });
            break;
          }
          setPlaying(false);
          const sessionId = playbackSessionIdRef.current;
          void reportPlayback("STOPPED", true, false, initialEngine.element, sessionId);
          void stopActiveSession(sessionId);
          break;
        }
        case "ERROR":
          if (activeEngine.kind === "native" && playbackPlan?.type === "DIRECT") {
            void requestServerFallback(event.error.message);
          } else {
            setFailedStreamUrl(streamUrl);
            setPlaybackFailure(classifyPlayerEngineFailure(event.error));
          }
          break;
        case "CAN_PLAY":
          syncSnapshot(activeEngine.snapshot(), true);
          break;
      }
    });
    const handlePerformance = (event: Event) => {
      if (cancelled) return;
      const performance = (event as CustomEvent<PlaybackPerformance | null>).detail;
      setFallbackSpeedX(performance && !performance.realtime ? performance.speedX : null);
    };
    const handleDurationChange = () => {
      if (!cancelled) syncSnapshot(activeEngine.snapshot(), true);
    };
    initialEngine.element.addEventListener("durationchange", handleDurationChange);
    // Only the native element can stall on an unreachable entry; the client engines read the origin
    // themselves and report their own failures.
    const ladderKind = playbackPlan?.type === "DIRECT" ? candidate?.kind : undefined;
    let metadataLoaded = false;
    const handleLoadedMetadata = () => {
      metadataLoaded = true;
      if (!cancelled && ladderKind && activeEngine.kind === "native") rememberCandidate(ladderKind);
    };
    initialEngine.element.addEventListener("loadedmetadata", handleLoadedMetadata);
    const candidateTimeout = ladderKind && hasNextCandidate
      ? window.setTimeout(() => {
        if (!cancelled && !metadataLoaded && activeEngine.kind === "native") {
          void requestServerFallback("candidate timed out");
        }
      }, DIRECT_CANDIDATE_TIMEOUT_MS)
      : undefined;
    const load = async () => {
      try {
        setClientEngineActive(false);
        if (playbackPlan?.type === "SERVER_HLS") {
          initialEngine.destroy();
          const { HlsVideoEngine } = await import("./hls-playback-engine");
          if (cancelled) return;
          activeEngine = new HlsVideoEngine(initialEngine.element);
          engineRef.current = activeEngine;
        } else {
          const useMkvFallback = isMatroskaSource(source)
            ? await shouldUseClientMkv(source, initialEngine.element)
            : false;
          const useHevcFallback =
            !useMkvFallback
            && (await shouldUseClientHevc(source, initialEngine.element));
          if (useMkvFallback || useHevcFallback) {
            setFallbackLoading(true);
            setClientEngineActive(true);
            if (cancelled) return;
            initialEngine.destroy();
            if (useMkvFallback) {
              const { ClientMkvEngine } = await import("./mkv-playback-engine");
              if (cancelled) return;
              const inputCodec = source?.streams?.find((stream) => (stream.type ?? "").toUpperCase() === "VIDEO")?.codec ?? "";
              activeEngine = new ClientMkvEngine(initialEngine.element, HEVC_RUNTIME_ASSETS, inputCodec);
            } else {
              const { ClientHevcEngine } = await import("./hevc-playback-engine");
              if (cancelled) return;
              activeEngine = new ClientHevcEngine(initialEngine.element, HEVC_RUNTIME_ASSETS);
            }
            engineRef.current = activeEngine;
            performanceElement = activeEngine.element;
            performanceElement.addEventListener(PLAYBACK_PERFORMANCE_EVENT, handlePerformance);
          }
        }
        if (cancelled) return;
        const engineSource = useClientEngine(activeEngine)
          ? clientMkvSourceUrl ?? streamUrl
          : streamUrl;
        await runtime.load(
          new LegacyPlaybackEngineAdapter(
            activeEngine,
            playbackPlan?.type === "SERVER_HLS" ? "hls" : undefined,
          ),
          {
            id: source?.id ?? "",
            url: engineSource,
            poster,
          },
        );
        if (!cancelled) await restoreEngineHandoff();
        if (!cancelled && activeEngine.performance)
          handlePerformance(
            new CustomEvent(PLAYBACK_PERFORMANCE_EVENT, {
              detail: activeEngine.performance,
            }),
          );
      } catch (cause) {
        if (!cancelled) {
          if (runtime.state.status === "FAILED") return;
          if (playbackPlan?.type === "DIRECT") {
            requestServerFallback(cause);
          } else {
            setFailedStreamUrl(streamUrl);
            setPlaybackFailure(classifyPlayerEngineFailure(cause));
          }
        }
      } finally {
        if (!cancelled) setFallbackLoading(false);
      }
    };
    void load();
    return () => {
      cancelled = true;
      window.clearTimeout(candidateTimeout);
      initialEngine.element.removeEventListener("loadedmetadata", handleLoadedMetadata);
      if ((remoteHttpSource || candidates.length > 1) && source?.id) {
        const video = activeEngine.element;
        const currentTime = Number.isFinite(video.currentTime)
          ? Math.max(0, video.currentTime)
          : currentTimeRef.current;
        if (
          playbackEngineHandoffRef.current?.playbackKey !== playbackKey
          || playbackEngineHandoffRef.current?.sourceId !== source.id
        ) {
          playbackEngineHandoffRef.current = {
            playbackKey,
            sourceId: source.id,
            currentTime,
            playing: !video.paused && !video.ended,
          };
        }
      }
      timelineScheduler.dispose();
      initialEngine.element.removeEventListener("durationchange", handleDurationChange);
      performanceElement?.removeEventListener(PLAYBACK_PERFORMANCE_EVENT, handlePerformance);
      removeRuntimeSubscription();
      runtime.destroy();
      if (runtimeRef.current === runtime) runtimeRef.current = null;
      if (engineRef.current === activeEngine) engineRef.current = null;
    };
  }, [candidate?.kind, candidates.length, hasNextCandidate, playbackKey, playbackPlan?.type, poster, remoteHttpSource, requestServerFallback, source, streamUrl]);
}
