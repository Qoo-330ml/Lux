// @vitest-environment jsdom

import { describe, expect, it, vi } from "vitest";
import { canUseClientMkvCaptionPipeline, canUseRemoteMkvCaptionSidecar, clientDecodeRequiresServerFallback, hasClientHevcCandidate, hasClientMkvCandidate, hasClientMkvHevcRuntime, isNativeMkvVideoCodec, isRemoteHttpStrmSource, resolveClientDecode, shouldUseClientHevc, shouldUseClientMkv } from "../src/features/player/playback-selection";

describe("playback selection", () => {
  const source = {
    id: "source-1",
    container: "mp4",
    sourceKind: "LOCAL_FILE",
    streams: [{ index: 0, type: "VIDEO", codec: "HEVC" }],
  };

  it("recognizes local MP4 HEVC sources as client-fallback candidates", () => {
    expect(hasClientHevcCandidate(source)).toBe(true);
    expect(hasClientHevcCandidate({ ...source, container: "mkv" })).toBe(false);
    expect(hasClientHevcCandidate({ ...source, sourceKind: "STRM_URL", externalUrl: "https://media.example.test/video.mp4" })).toBe(true);
    expect(hasClientHevcCandidate({ ...source, streams: [{ index: 0, type: "VIDEO", codec: "dvh1.05.06" }] })).toBe(false);
  });

  it("recognizes MKV HEVC with AAC as a separate client-fallback candidate", async () => {
    const mkv = { ...source, container: "mkv", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC", details: { width: 1920, height: 1080 } },
      { index: 1, type: "AUDIO", codec: "AAC" },
    ] };
    expect(hasClientMkvCandidate(mkv)).toBe(true);
    expect(hasClientMkvCandidate({ ...mkv, streams: [{ index: 0, type: "VIDEO", codec: "HEVC" }, { index: 1, type: "AUDIO", codec: "DTS" }] })).toBe(false);
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder { static isConfigSupported() { return Promise.resolve({ supported: true }); } });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");
    expect(await shouldUseClientMkv(mkv, video)).toBe(true);
  });

  it("recognizes ffprobe's matroska,webm container label as an MKV fallback candidate", () => {
    const matroska = { ...source, container: "matroska,webm", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC" },
      { index: 1, type: "AUDIO", codec: "AAC" },
    ] };

    expect(hasClientMkvCandidate(matroska)).toBe(true);
  });

  it("recognizes a WebM-only container label for remote caption playback", () => {
    const webm = { ...source, container: "webm", streams: [
      { index: 0, type: "VIDEO", codec: "VP9" },
      { index: 1, type: "AUDIO", codec: "OPUS" },
    ] };

    expect(hasClientMkvCandidate(webm)).toBe(true);
  });

  it("recognizes only HTTP(S) URL STRM sources as browser-direct media", () => {
    expect(isRemoteHttpStrmSource({
      ...source,
      sourceKind: "STRM_URL",
      externalUrl: "https://media.example.test/video.mkv",
    })).toBe(true);
    expect(isRemoteHttpStrmSource({
      ...source,
      sourceKind: "STRM_URL",
      externalUrl: "/CloudNAS/video.mkv",
    })).toBe(false);
    expect(isRemoteHttpStrmSource({ ...source, sourceKind: "LOCAL_FILE" })).toBe(false);
  });

  it("accepts an MKV when a supported AAC track follows DTS and E-AC-3 tracks", () => {
    const mkv = { ...source, container: "mkv", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC" },
      { index: 1, type: "AUDIO", codec: "DTS" },
      { index: 2, type: "AUDIO", codec: "EAC3" },
      { index: 3, type: "AUDIO", codec: "AAC" },
    ] };
    expect(hasClientMkvCandidate(mkv)).toBe(true);
  });

  it("selects E-AC-3 only when HEVC and E-AC-3 share an MSE", async () => {
    const mkv = { ...source, container: "mkv", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC" },
      { index: 1, type: "AUDIO", codec: "EAC3" },
    ] };
    expect(hasClientMkvCandidate(mkv)).toBe(true);
    vi.stubGlobal("MediaSource", { isTypeSupported: (mime: string) => !mime.includes("ec-3") });
    vi.stubGlobal("VideoEncoder", undefined);
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");
    expect(await shouldUseClientMkv(mkv, video)).toBe(false);

    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    expect(await shouldUseClientMkv(mkv, video)).toBe(true);
  });

  it("does not select the AAC-only SDR path for an E-AC-3-only MKV", async () => {
    const mkv = { ...source, container: "mkv", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC" },
      { index: 1, type: "AUDIO", codec: "EAC3" },
    ] };
    vi.stubGlobal("MediaSource", { isTypeSupported: (mime: string) => mime.includes("avc1") });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder { static isConfigSupported() { return Promise.resolve({ supported: true }); } });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");
    expect(await shouldUseClientMkv(mkv, video)).toBe(false);
  });

  it("uses the fallback only when native HEVC is unavailable", async () => {
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder { static isConfigSupported() { return Promise.resolve({ supported: true }); } });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");
    expect(await shouldUseClientHevc(source, video)).toBe(true);

    vi.spyOn(video, "canPlayType").mockReturnValue("probably");
    expect(await shouldUseClientHevc(source, video)).toBe(false);
  });

  it("does not select fallback when the browser lacks a required runtime API", async () => {
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", undefined);
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");

    expect(await shouldUseClientHevc(source, video)).toBe(false);
  });

  it("selects the MKV path when Safari can MSE-play HEVC without VideoEncoder", async () => {
    const mkv = { ...source, container: "mkv", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC", details: { width: 3840, height: 1636, averageFrameRate: 60 } },
      { index: 1, type: "AUDIO", codec: "AAC" },
    ] };
    vi.stubGlobal("MediaSource", { isTypeSupported: (mime: string) => mime.includes("hvc1") });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", undefined);
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");

    expect(hasClientMkvHevcRuntime()).toBe(true);
    expect(await shouldUseClientMkv(mkv, video)).toBe(true);
  });

  it("keeps native Matroska playback as the first path when the browser accepts it", async () => {
    const mkv = { ...source, container: "mkv", streams: [
      { index: 0, type: "VIDEO", codec: "H264" },
      { index: 1, type: "AUDIO", codec: "AAC" },
    ] };
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("probably");

    expect(await shouldUseClientMkv(mkv, video)).toBe(false);
  });

  it("keeps the client MKV pipeline for captions when native MKV playback is available", async () => {
    const mkv = { ...source, container: "mkv", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC" },
      { index: 1, type: "AUDIO", codec: "EAC3" },
      { index: 2, type: "SUBTITLE", codec: "ASS" },
    ] };
    vi.stubGlobal("MediaSource", {
      // The client remux now also requires the audio codec itself to be decodable, so a real
      // browser answers both the pair and the audio-only query for E-AC-3.
      isTypeSupported: (mime: string) => mime.includes("hvc1.2.4.L120") || mime.includes("avc1") || mime.includes("ec-3"),
    });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder {
      static isConfigSupported() { return Promise.resolve({ supported: true }); }
    });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("probably");

    expect(await shouldUseClientMkv(mkv, video)).toBe(false);
    expect(await shouldUseClientMkv(mkv, video, { requireCaptionPipeline: true })).toBe(true);
  });

  it("rejects remote caption remux when Chrome cannot MSE the HEVC plus E-AC-3 pair", () => {
    const mkv = { ...source, container: "mkv", sourceKind: "STRM_URL", externalUrl: "https://media.example.test/video.mkv", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC" },
      { index: 1, type: "AUDIO", codec: "EAC3" },
      { index: 2, type: "SUBTITLE", codec: "ASS" },
    ] };
    vi.stubGlobal("MediaSource", {
      isTypeSupported: (mime: string) => mime.includes("hvc1") && !mime.includes("ec-3"),
    });

    expect(canUseClientMkvCaptionPipeline(mkv)).toBe(false);
  });

  it("keeps remote text captions available through the native-audio sidecar when MSE rejects the audio pair", () => {
    const mkv = { ...source, container: "mkv", sourceKind: "STRM_URL", externalUrl: "https://media.example.test/video.mkv", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC" },
      { index: 1, type: "AUDIO", codec: "EAC3" },
      { index: 2, type: "SUBTITLE", codec: "ASS", isExternal: false },
    ] };

    vi.stubGlobal("MediaSource", {
      isTypeSupported: (mime: string) => mime.includes("hvc1") && !mime.includes("ec-3"),
    });

    expect(canUseClientMkvCaptionPipeline(mkv)).toBe(false);
    expect(canUseRemoteMkvCaptionSidecar(mkv)).toBe(true);
  });

  it("selects MKV AC-3 only when HEVC and AC-3 share an MSE", async () => {
    const mkv = { ...source, container: "mkv", streams: [
      { index: 0, type: "VIDEO", codec: "HEVC" },
      { index: 1, type: "AUDIO", codec: "AC3" },
    ] };
    expect(hasClientMkvCandidate(mkv)).toBe(true);
    vi.stubGlobal("MediaSource", { isTypeSupported: (mime: string) => !mime.includes("ac-3") });
    vi.stubGlobal("VideoEncoder", undefined);
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");
    expect(await shouldUseClientMkv(mkv, video)).toBe(false);

    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    expect(await shouldUseClientMkv(mkv, video)).toBe(true);
  });

  it("does not select fallback when H.264 VideoEncoder rejects the source configuration", async () => {
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder { static isConfigSupported() { return Promise.resolve({ supported: false }); } });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");

    expect(await shouldUseClientHevc(source, video)).toBe(false);
  });

  it("probes the source's real resolution for the WASM soft decoder", async () => {
    let config: Record<string, unknown> | undefined;
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder {
      static isConfigSupported(nextConfig: Record<string, unknown>) {
        config = nextConfig;
        return Promise.resolve({ supported: true });
      }
    });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");

    expect(await shouldUseClientHevc({
      ...source,
      bitrate: 8_000_000,
      streams: [{ index: 0, type: "VIDEO", codec: "HEVC", details: { Width: 1920, Height: 1080, AverageFrameRate: 24 } }],
    }, video)).toBe(true);
    expect(config).toMatchObject({ codec: "avc1.64002a", width: 1920, height: 1080, framerate: 24 });
  });

  it("never runs the WASM soft decoder on a 4K HEVC source", async () => {
    let probeCalls = 0;
    vi.stubGlobal("MediaSource", { isTypeSupported: () => true });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder {
      static isConfigSupported() {
        probeCalls += 1;
        return Promise.resolve({ supported: true });
      }
    });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");

    expect(await shouldUseClientHevc({
      ...source,
      streams: [{ index: 0, type: "VIDEO", codec: "HEVC", details: { Width: 3840, Height: 2160, AverageFrameRate: 30 } }],
    }, video)).toBe(false);
    expect(probeCalls).toBe(0);
  });
});

describe("client decode realtime gate", () => {
  const hevcMkv = (details: Record<string, unknown>, audio = "AAC") => ({
    id: "source-gate",
    container: "matroska,webm",
    sourceKind: "LOCAL_FILE",
    streams: [
      { index: 0, type: "VIDEO", codec: "hevc", details },
      { index: 1, type: "AUDIO", codec: audio },
    ],
  });
  const h264Mkv = (details: Record<string, unknown>, audio = "AAC") => ({
    id: "source-h264",
    container: "matroska",
    sourceKind: "LOCAL_FILE",
    streams: [
      { index: 0, type: "VIDEO", codec: "h264", details },
      { index: 1, type: "AUDIO", codec: audio },
    ],
  });
  const stubRuntime = (support: (mime: string) => boolean) => {
    vi.stubGlobal("MediaSource", { isTypeSupported: support });
    vi.stubGlobal("Worker", class Worker {});
    vi.stubGlobal("VideoEncoder", class VideoEncoder {
      static isConfigSupported() { return Promise.resolve({ supported: true }); }
      static probe: unknown;
    });
    const video = document.createElement("video");
    vi.spyOn(video, "canPlayType").mockReturnValue("");
    return video;
  };

  it("never sends a 4K HEVC source to the WASM soft decoder", async () => {
    const video = stubRuntime((mime) => mime.includes("avc1"));
    const decision = await resolveClientDecode(hevcMkv({ Width: 3840, Height: 2160, PixelFormat: "yuv420p10le", Profile: "Main 10", Level: 153 }), video);

    expect(decision).toEqual({ engine: null, rejected: "client-resolution-too-high" });
    expect(clientDecodeRequiresServerFallback(decision)).toBe(true);
  });

  it("keeps the native HEVC remux for 4K HEVC when the browser decodes HEVC itself", async () => {
    const video = stubRuntime((mime) => mime.includes("hvc1"));
    const decision = await resolveClientDecode(hevcMkv({ Width: 3840, Height: 2160, PixelFormat: "yuv420p10le", Profile: "Main 10", Level: 153 }), video);

    expect(decision).toEqual({ engine: "client-mkv", rejected: null });
    expect(clientDecodeRequiresServerFallback(decision)).toBe(false);
  });

  it("keeps the WASM soft decoder for a 1080p HEVC source", async () => {
    const video = stubRuntime((mime) => mime.includes("avc1"));
    const decision = await resolveClientDecode(hevcMkv({ Width: 1920, Height: 1080, PixelFormat: "yuv420p10le", Level: 120 }), video);

    expect(decision).toEqual({ engine: "client-mkv", rejected: null });
  });

  it("probes a 10-bit H.264 source with its High 10 codec string", async () => {
    let probed: string[] = [];
    const video = stubRuntime((mime) => {
      probed = [...probed, mime];
      return mime.includes("avc1.64002a");
    });

    const decision = await resolveClientDecode(h264Mkv({ Width: 1920, Height: 1080, PixelFormat: "yuv420p10le" }), video);

    expect(probed.some((mime) => mime.includes("avc1.6E002a"))).toBe(true);
    expect(decision).toEqual({ engine: null, rejected: "client-codec-unsupported" });
    expect(clientDecodeRequiresServerFallback(decision)).toBe(true);
  });

  it("uses the client remux when MSE really decodes 10-bit H.264", async () => {
    const video = stubRuntime((mime) => mime.includes("avc1.6E002a"));

    expect(await resolveClientDecode(h264Mkv({ Width: 1920, Height: 1080, PixelFormat: "yuv420p10le" }), video))
      .toEqual({ engine: "client-mkv", rejected: null });
  });

  it("keeps the 8-bit 1080p H.264 remux unchanged", async () => {
    const video = stubRuntime((mime) => mime.includes("avc1.64002a"));

    expect(await resolveClientDecode(h264Mkv({ Width: 1920, Height: 1080, PixelFormat: "yuv420p" }), video))
      .toEqual({ engine: "client-mkv", rejected: null });
  });

  it("refuses an AC-3 passthrough when MSE has no AC-3 decoder", async () => {
    const video = stubRuntime((mime) => mime.includes("avc1") && !mime.includes("ac-3"));

    expect(await resolveClientDecode(h264Mkv({ Width: 1920, Height: 1080 }, "AC3"), video))
      .toEqual({ engine: null, rejected: "client-codec-unsupported" });
  });

  it("keeps the AC-3 passthrough when MSE decodes AC-3", async () => {
    const video = stubRuntime((mime) => mime.includes("avc1") || mime.includes("ac-3"));

    expect(await resolveClientDecode(h264Mkv({ Width: 1920, Height: 1080 }, "AC3"), video))
      .toEqual({ engine: "client-mkv", rejected: null });
  });

  it("leaves a VP9 Matroska the element can open to native playback", async () => {
    const mkv = {
      id: "source-vp9",
      container: "mkv",
      sourceKind: "LOCAL_FILE",
      streams: [
        { index: 0, type: "VIDEO", codec: "vp9", details: { Width: 1280, Height: 720 } },
        { index: 1, type: "AUDIO", codec: "opus" },
      ],
    };
    const video = stubRuntime(() => true);
    vi.spyOn(video, "canPlayType").mockReturnValue("probably");

    expect(await resolveClientDecode(mkv, video)).toEqual({ engine: null, rejected: "native-playback" });
    expect(clientDecodeRequiresServerFallback({ engine: null, rejected: "native-playback" })).toBe(false);
  });

  it("uses the client remux for VP9 when the element cannot open Matroska", async () => {
    const mkv = {
      id: "source-vp9",
      container: "mkv",
      sourceKind: "LOCAL_FILE",
      streams: [
        { index: 0, type: "VIDEO", codec: "vp9", details: { Width: 1280, Height: 720 } },
        { index: 1, type: "AUDIO", codec: "opus" },
      ],
    };
    const video = stubRuntime((mime) => mime.includes("vp09"));

    expect(await resolveClientDecode(mkv, video)).toEqual({ engine: "client-mkv", rejected: null });
  });

  it("refuses the native Matroska path for a video codec outside the whitelist", async () => {
    // Chromium answers “maybe” for a Matroska whose video track is MPEG-4 Part 2 and then plays only
    // the audio track, so the element's own answer cannot be trusted for an unknown codec.
    const mpeg4 = {
      id: "source-mpeg4",
      container: "mkv",
      sourceKind: "LOCAL_FILE",
      streams: [
        { index: 0, type: "VIDEO", codec: "mpeg4" },
        { index: 1, type: "AUDIO", codec: "mp3" },
      ],
    };
    const video = stubRuntime(() => false);
    vi.spyOn(video, "canPlayType").mockReturnValue("probably");

    expect(await resolveClientDecode(mpeg4, video)).toEqual({ engine: null, rejected: "native-codec-unsupported" });
    expect(clientDecodeRequiresServerFallback({ engine: null, rejected: "native-codec-unsupported" })).toBe(true);
  });

  it("leaves a remote STRM Matroska with an unknown codec to the element", async () => {
    // A remote `.strm` source has no server plan to fall back to, so it must keep whatever the
    // element does instead of turning into a failure.
    const remote = {
      id: "source-mpeg4-remote",
      container: "mkv",
      sourceKind: "STRM_URL",
      externalUrl: "https://media.example.test/video.mkv",
      streams: [
        { index: 0, type: "VIDEO", codec: "mpeg4" },
        { index: 1, type: "AUDIO", codec: "mp3" },
      ],
    };
    const video = stubRuntime(() => false);

    expect(await resolveClientDecode(remote, video)).toEqual({ engine: null, rejected: "no-candidate" });
  });

  it("reports no client candidate for a non-Matroska source the client path never handled", async () => {
    const mpeg4 = {
      id: "source-mpeg4-mp4",
      container: "mov,mp4,m4a,3gp,3g2,mj2",
      sourceKind: "LOCAL_FILE",
      streams: [{ index: 0, type: "VIDEO", codec: "mpeg4" }],
    };
    const video = stubRuntime(() => false);

    expect(await resolveClientDecode(mpeg4, video)).toEqual({ engine: null, rejected: "no-candidate" });
    expect(clientDecodeRequiresServerFallback({ engine: null, rejected: "no-candidate" })).toBe(false);
  });

  it("never hints an unknown Matroska video codec as avc1", async () => {
    // `hvc1` without a profile suffix is in the remux candidate list but has no MSE string; the old
    // fallback asked the element about `codecs="avc1"` and let an HEVC remux through as native.
    const mkv = {
      id: "source-hvc1",
      container: "mkv",
      sourceKind: "LOCAL_FILE",
      streams: [
        { index: 0, type: "VIDEO", codec: "hvc1" },
        { index: 1, type: "AUDIO", codec: "AAC" },
      ],
    };
    const video = stubRuntime(() => false);
    const asked: string[] = [];
    vi.spyOn(video, "canPlayType").mockImplementation((mime: string) => {
      asked.push(mime);
      return "maybe";
    });

    const decision = await resolveClientDecode(mkv, video);

    expect(asked.filter((mime) => mime.includes("avc1"))).toEqual([]);
    expect(decision).toEqual({ engine: null, rejected: "client-codec-unsupported" });
  });
});

describe("native Matroska video whitelist", () => {
  it("trusts only h264, hevc, vp9 and av1", () => {
    for (const codec of ["h264", "H264", "avc", "avc1", "avc1.640028", "hevc", "h265", "hvc1.2.4.L120.B0", "hev1.1.6.L120.B0", "vp9", "vp09.00.10.08", "av1", "av01.0.04M.08"]) {
      expect(isNativeMkvVideoCodec(codec), codec).toBe(true);
    }
    for (const codec of ["mpeg4", "mpeg2video", "msmpeg4v3", "vc1", "wmv3", "mjpeg", "theora", "hvc1", "dvh1.05.06", ""]) {
      expect(isNativeMkvVideoCodec(codec), codec).toBe(false);
    }
    expect(isNativeMkvVideoCodec(null)).toBe(false);
    expect(isNativeMkvVideoCodec(undefined)).toBe(false);
  });
});
