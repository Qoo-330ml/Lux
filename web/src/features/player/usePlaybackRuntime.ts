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
import {
  clientDecodeRequiresServerFallback,
  resolveClientDecode,
} from "./playback-selection";
import {
  CLIENT_FIRST_SEGMENT_TIMEOUT_MS,
  DIRECT_FIRST_FRAME_TIMEOUT_MS,
  NATIVE_SILENT_POLL_MS,
  evaluateClientDecodeWatchdog,
  evaluateDirectPlayWatchdog,
  evaluateNativeSilentFailure,
  markClientDecodeFirstSegment,
  recordClientDecodeSpeedSample,
  sampleNativeSilentPlayback,
  startClientDecodeWatchdog,
  startNativeSilentPlaybackClock,
  type ClientDecodeWatchdog,
} from "./client-decode-watchdog";
import {
  clientDecodeFallbackDiagnostic,
  playbackFallbackDiagnostic,
  type ClientDecodeFallbackReason,
  type PlaybackFallbackOptions,
} from "./client-decode-fallback";
import { LegacyPlaybackEngineAdapter } from "./core/legacy-engine-adapter";
import { LuxPlayerRuntime } from "./core/player-runtime";
import { classifyPlayerEngineFailure } from "./components/player-diagnostics";
import { createPlaybackTimelineScheduler } from "./player-timeline-scheduler";
import {
  attemptAutoStartPlayback,
  createAutoStartPlaybackGate,
  settleAutoStartPlaybackAfterFailure,
} from "./player-autostart";
import { TICKS_PER_SECOND, useClientEngine } from "./player-page-helpers";
import type { PlaybackReporting } from "./usePlaybackReporting";

export const TIMELINE_UI_UPDATE_INTERVAL_MS = 100;

/** Engine failures arrive as `Error` or as the runtime's `{ code, message }` error payload. */
function playbackFailureDetail(cause: unknown) {
  if (cause instanceof Error) return cause.message;
  if (cause && typeof cause === "object" && typeof (cause as { message?: unknown }).message === "string") {
    return (cause as { message: string }).message;
  }
  return typeof cause === "string" ? cause : undefined;
}

/** Chromium's count of decoded audio bytes; `undefined` when the browser does not expose it. */
function nativeAudioDecodedBytes(element: HTMLVideoElement) {
  const value = (element as HTMLVideoElement & { webkitAudioDecodedByteCount?: unknown })
    .webkitAudioDecodedByteCount;
  return typeof value === "number" ? value : undefined;
}

/**
 * Safari's audio track list length; `undefined` when it cannot be used as a silence signal.
 *
 * Chromium reports the truthful decoded-byte counter above, so its track list is ignored, and an
 * element that has not reached `HAVE_CURRENT_DATA` is still buffering rather than decoding silence.
 */
function nativeAudioTrackCount(element: HTMLVideoElement) {
  if (typeof nativeAudioDecodedBytes(element) === "number") return undefined;
  if (element.readyState < 2) return undefined;
  const length = (element as HTMLVideoElement & { audioTracks?: { length?: unknown } })
    .audioTracks?.length;
  return typeof length === "number" ? length : undefined;
}

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
  /** Playback reached the media end without looping; the page decides what is next. */
  onPlaybackEnded?: () => void;
  /** One-shot request: start playing once this session finished loading. */
  autoStartPlayback?: boolean;
  /** The viewer resumed playback or moved the playhead inside this episode. */
  onPlaybackInterrupted?: () => void;
  /** True when this same episode reloads (server fallback or candidate retry). */
  requestServerFallback: (
    reason?: unknown,
    clientDecodeFallback?: ClientDecodeFallbackReason,
    options?: PlaybackFallbackOptions,
  ) => Promise<boolean>;
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
  onPlaybackEnded,
  autoStartPlayback,
  onPlaybackInterrupted,
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
  // The ended callback is read through a ref so a changing page callback never
  // re-runs the engine loading effect below.
  const onPlaybackEndedRef = useRef(onPlaybackEnded);
  const onPlaybackInterruptedRef = useRef(onPlaybackInterrupted);
  // The auto start request survives the page stripping the router marker right
  // after mount, and is consumed once so a source switch does not replay it.
  const autoStartGateRef = useRef(createAutoStartPlaybackGate(autoStartPlayback === true));

  useEffect(() => {
    onPlaybackEndedRef.current = onPlaybackEnded;
  }, [onPlaybackEnded]);

  useEffect(() => {
    onPlaybackInterruptedRef.current = onPlaybackInterrupted;
  }, [onPlaybackInterrupted]);

  useEffect(() => {
    if (autoStartPlayback) autoStartGateRef.current.request();
  }, [autoStartPlayback]);

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
          onPlaybackInterruptedRef.current?.();
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
          onPlaybackInterruptedRef.current?.();
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
          onPlaybackEndedRef.current?.();
          break;
        }
        case "ERROR":
          if (playbackPlan?.type === "DIRECT") {
            // Both engines land here: a native media error, and a client engine that reported a
            // failed MSE append or remux worker error after playback started.
            void requestDirectFallback(
              event.error,
              useClientEngine(activeEngine) ? "unviable" : undefined,
            ).catch(() => undefined);
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
      if (clientWatchdog === null || !performance) return;
      clientWatchdog = recordClientDecodeSpeedSample(clientWatchdog, performance.speedX);
      const verdict = evaluateClientDecodeWatchdog(clientWatchdog, Date.now());
      // `slow` keeps the existing “客户端解码速度低于实时” banner above; only a clearly-too-slow
      // measurement (or a missing first segment) is worth an automatic server fallback.
      if (verdict.action === "fallback") {
        requestClientDecodeFallback(verdict.reason, `speedX ${performance.speedX.toFixed(2)}`);
      }
    };
    const handleDurationChange = () => {
      if (!cancelled) syncSnapshot(activeEngine.snapshot(), true);
    };
    initialEngine.element.addEventListener("durationchange", handleDurationChange);
    // Only the native element can stall on an unreachable entry; the client engines read the origin
    // themselves and report their own failures.
    const ladderKind = playbackPlan?.type === "DIRECT" ? candidate?.kind : undefined;
    let metadataLoaded = false;
    // The silent-failure clock only counts playback after metadata, so a slow first byte cannot look
    // like a source the element fails to decode.
    let silentClock = startNativeSilentPlaybackClock();
    const handleLoadedMetadata = () => {
      metadataLoaded = true;
      silentClock = startNativeSilentPlaybackClock();
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
    // The client engines await browser-owned steps with no timeout of their own (module Worker
    // readiness, the first remuxed segment, MSE `updateend`). Without a watchdog a Safari/Edge stall
    // stays on “正在准备客户端解码…” forever instead of reaching server transcoding.
    let clientWatchdog: ClientDecodeWatchdog | null = null;
    let firstSegmentTimeout: number | undefined;
    let fallbackRequested = false;
    /**
     * One server-side retry per load. Any engine failure on a `DIRECT` plan (native media error,
     * client MSE append rejection, remux worker error, first-segment timeout) asks the server for a
     * transcoding session; only a refused fallback ends in the engine-failure screen.
     */
    const requestDirectFallback = async (
      cause: unknown,
      reason?: ClientDecodeFallbackReason,
      detail?: string,
      options?: PlaybackFallbackOptions,
    ) => {
      if (cancelled || fallbackRequested) return false;
      fallbackRequested = true;
      window.clearTimeout(firstSegmentTimeout);
      window.clearTimeout(nativeStallTimeout);
      const message = detail ?? playbackFailureDetail(cause);
      // Web Inspector has no other trace of this decision; the copy never carries media URLs. Remote
      // `.strm` sources only rotate Direct Play candidates, so they must not claim a transcode.
      if (!remoteHttpSource) {
        console.warn(reason
          ? clientDecodeFallbackDiagnostic(reason, message)
          : playbackFallbackDiagnostic(message));
      }
      const reloadPending = await requestServerFallback(cause, reason, options);
      if (!cancelled) settleAutoStartPlaybackAfterFailure(autoStartGateRef.current, reloadPending);
      return reloadPending;
    };
    const requestClientDecodeFallback = (
      reason: ClientDecodeFallbackReason,
      detail?: string,
      options?: PlaybackFallbackOptions,
    ) => {
      void requestDirectFallback(new Error(`client decode fallback: ${reason}`), reason, detail, options)
        .catch(() => undefined);
    };
    const startClientWatchdog = () => {
      clientWatchdog = startClientDecodeWatchdog(Date.now());
      firstSegmentTimeout = window.setTimeout(() => {
        if (cancelled || clientWatchdog === null) return;
        const verdict = evaluateClientDecodeWatchdog(clientWatchdog, Date.now());
        if (verdict.action === "fallback") requestClientDecodeFallback(verdict.reason);
      }, CLIENT_FIRST_SEGMENT_TIMEOUT_MS);
    };
    const markFirstPlayableSegment = () => {
      if (clientWatchdog === null) return;
      clientWatchdog = markClientDecodeFirstSegment(clientWatchdog, Date.now());
      window.clearTimeout(firstSegmentTimeout);
    };
    // A native Direct Play element that never reaches HAVE_CURRENT_DATA is stuck too: Chromium opens
    // Matroska whose video codec it cannot decode without firing `error` or `loadedmetadata`. Remote
    // `.strm` sources are exempt: they have no server transcoding to fall back to, so a slow download
    // must keep buffering instead of turning into a failure.
    const nativeWatchdog = playbackPlan?.type === "DIRECT" && !hasNextCandidate && !remoteHttpSource
      ? startClientDecodeWatchdog(Date.now())
      : null;
    const nativeStallTimeout = nativeWatchdog === null
      ? undefined
      : window.setTimeout(() => {
        if (cancelled || fallbackRequested || activeEngine.kind !== "native") return;
        if (initialEngine.element.readyState >= 2) return;
        if (!evaluateDirectPlayWatchdog(nativeWatchdog, Date.now())) return;
        fallbackRequested = true;
        console.warn(clientDecodeFallbackDiagnostic("stalled", "direct play produced no frame"));
        void requestServerFallback(new Error("direct play produced no frame"), "stalled")
          .then((reloadPending) => {
            if (!cancelled) settleAutoStartPlaybackAfterFailure(autoStartGateRef.current, reloadPending);
          })
          .catch(() => undefined);
      }, DIRECT_FIRST_FRAME_TIMEOUT_MS);
    // Native playback can fail *silently*: a Chromium element opens a Matroska whose video codec it
    // cannot decode and plays only the audio track (`currentTime` advances, `readyState` reaches 4,
    // `videoWidth` stays 0), and an Edge without the Dolby extension claims AC-3 while decoding no
    // audio frame. Neither fires `error`, so without this poll the viewer keeps a silent black
    // surface and the session never asks for server transcoding.
    const hasVideoStream = (source?.streams ?? [])
      .some((stream) => (stream.type ?? "").toUpperCase() === "VIDEO");
    const hasAudioStream = (source?.streams ?? [])
      .some((stream) => (stream.type ?? "").toUpperCase() === "AUDIO");
    const silentWatchdogInterval = playbackPlan?.type === "DIRECT" && !remoteHttpSource
      ? window.setInterval(() => {
        if (cancelled || fallbackRequested || activeEngine.kind !== "native" || !metadataLoaded) return;
        const element = initialEngine.element;
        silentClock = sampleNativeSilentPlayback(silentClock, {
          nowMs: Date.now(),
          currentTime: element.currentTime,
          paused: element.paused,
          seeking: element.seeking,
        });
        const failure = evaluateNativeSilentFailure({
          hasVideoStream,
          hasAudioStream,
          playedMs: silentClock.playedMs,
          videoWidth: element.videoWidth,
          audioDecodedBytes: nativeAudioDecodedBytes(element),
          audioTrackCount: nativeAudioTrackCount(element),
        });
        if (failure === "silent-video") {
          requestClientDecodeFallback(failure, "native element produced no video frame");
        } else if (failure === "silent-audio") {
          // The element decoded no audio at all, so the retry must re-encode the audio track even
          // when the browser claims to support its codec.
          requestClientDecodeFallback(failure, "native element decoded no audio", {
            forceAudioTranscode: true,
          });
        }
      }, NATIVE_SILENT_POLL_MS)
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
          const decision = await resolveClientDecode(source, initialEngine.element);
          if (cancelled) return;
          if (decision.engine !== null) {
            setFallbackLoading(true);
            setClientEngineActive(true);
            initialEngine.destroy();
            if (decision.engine === "client-mkv") {
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
            startClientWatchdog();
          } else if (clientDecodeRequiresServerFallback(decision) && playbackPlan?.type === "DIRECT") {
            // The browser cannot decode what the server planned to send, so ask the server for a
            // transcoding session instead of waiting for an element that will never play.
            fallbackRequested = true;
            window.clearTimeout(nativeStallTimeout);
            console.warn(clientDecodeFallbackDiagnostic("unviable", decision.rejected));
            const reloadPending = await requestServerFallback(
              new Error(`client decode unavailable: ${decision.rejected}`),
              "unviable",
            );
            if (!cancelled) settleAutoStartPlaybackAfterFailure(autoStartGateRef.current, reloadPending);
            return;
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
        if (!cancelled) markFirstPlayableSegment();
        if (!cancelled) await restoreEngineHandoff();
        if (!cancelled && autoStartGateRef.current.peek()) {
          // The viewer's one-shot request is finished once playback started or once
          // the browser policy rejected it. A real error propagates to the failure
          // handling below, which decides whether the request survives for a
          // same-episode reload (see settleAutoStartPlaybackAfterFailure). A policy
          // rejection leaves the player paused on the normal play affordance.
          await attemptAutoStartPlayback(() => runtime.play());
          autoStartGateRef.current.consume();
        }
        if (!cancelled && activeEngine.performance)
          handlePerformance(
            new CustomEvent(PLAYBACK_PERFORMANCE_EVENT, {
              detail: activeEngine.performance,
            }),
          );
      } catch (cause) {
        // A rejected engine (MSE append failure, `isTypeSupported` rejection, worker error) on a
        // Direct Play plan is retried once server-side instead of stopping on the engine-failure
        // screen. `requestDirectFallback` is idempotent, so a prior ERROR event or watchdog cannot
        // double-request; the fallback itself settles the one-shot auto start.
        if (playbackPlan?.type === "DIRECT") {
          await requestDirectFallback(
            cause,
            useClientEngine(activeEngine) ? "unviable" : undefined,
          );
        } else if (!cancelled) {
          settleAutoStartPlaybackAfterFailure(autoStartGateRef.current, false);
          if (!fallbackRequested) {
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
      window.clearTimeout(firstSegmentTimeout);
      window.clearTimeout(nativeStallTimeout);
      window.clearInterval(silentWatchdogInterval);
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
