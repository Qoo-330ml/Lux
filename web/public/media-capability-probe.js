export const CODEC_PRESETS = [
  {
    id: "hevc-main-4k",
    label: "HEVC Main · 4K · SDR",
    mime: 'video/mp4; codecs="hvc1.1.6.L150.B0"',
    codec: "hvc1.1.6.L150.B0",
    width: 3840,
    height: 2160,
    bitDepth: 8,
    hdr: "SDR",
    bitrate: 25_000_000,
    framerate: 30,
  },
  {
    id: "hevc-main10-hdr10-4k",
    label: "HEVC Main10 · 4K · HDR10",
    mime: 'video/mp4; codecs="hvc1.2.4.L153.B0"',
    codec: "hvc1.2.4.L153.B0",
    width: 3840,
    height: 2160,
    bitDepth: 10,
    hdr: "HDR10",
    bitrate: 35_000_000,
    framerate: 30,
  },
  {
    id: "avc-4k",
    label: "H.264 High · 4K · SDR（基准）",
    mime: 'video/mp4; codecs="avc1.640033"',
    codec: "avc1.640033",
    width: 3840,
    height: 2160,
    bitDepth: 8,
    hdr: "SDR",
    bitrate: 25_000_000,
    framerate: 30,
  },
];

export function normalizeNativeSupport(value) {
  if (value === "probably") return "supported";
  if (value === "maybe") return "maybe";
  return "unsupported";
}

export function buildDecoderConfig(input) {
  const codec = String(input.codec ?? "").trim();
  if (!codec) throw new Error("codec is required");

  return {
    codec,
    codedWidth: positiveInteger(input.width, "width"),
    codedHeight: positiveInteger(input.height, "height"),
    bitrate: positiveNumber(input.bitrate, "bitrate"),
    framerate: positiveNumber(input.framerate, "framerate"),
    hardwareAcceleration: "prefer-hardware",
  };
}

function positiveInteger(value, name) {
  const number = Number(value);
  if (!Number.isInteger(number) || number <= 0) throw new Error(`${name} must be a positive integer`);
  return number;
}

function positiveNumber(value, name) {
  const number = Number(value);
  if (!Number.isFinite(number) || number <= 0) throw new Error(`${name} must be positive`);
  return number;
}

async function probeMediaCapabilities({ mime, width, height, bitrate, framerate }) {
  if (!navigator.mediaCapabilities?.decodingInfo) return { available: false };
  try {
    const result = await navigator.mediaCapabilities.decodingInfo({
      type: "file",
      video: { contentType: mime, width, height, bitrate, framerate },
    });
    return {
      available: true,
      supported: result.supported,
      smooth: result.smooth,
      powerEfficient: result.powerEfficient,
    };
  } catch (error) {
    return { available: true, error: error instanceof Error ? error.message : String(error) };
  }
}

async function probeWebCodecs(input) {
  if (!globalThis.VideoDecoder?.isConfigSupported) return { available: false };
  try {
    const result = await VideoDecoder.isConfigSupported(buildDecoderConfig(input));
    return {
      available: true,
      supported: result.supported,
      config: result.config,
    };
  } catch (error) {
    return { available: true, error: error instanceof Error ? error.message : String(error) };
  }
}

function waitForVideoMetadata(video, timeoutMs = 10_000) {
  return new Promise((resolve) => {
    const timeout = window.setTimeout(() => {
      cleanup();
      resolve({ ok: false, error: `metadata timeout after ${timeoutMs}ms` });
    }, timeoutMs);
    const cleanup = () => {
      window.clearTimeout(timeout);
      video.removeEventListener("loadedmetadata", onMetadata);
      video.removeEventListener("error", onError);
    };
    const onMetadata = () => {
      cleanup();
      resolve({
        ok: true,
        width: video.videoWidth,
        height: video.videoHeight,
        duration: Number.isFinite(video.duration) ? video.duration : null,
      });
    };
    const onError = () => {
      cleanup();
      resolve({
        ok: false,
        error: video.error?.message || `MediaError ${video.error?.code ?? "unknown"}`,
      });
    };
    video.addEventListener("loadedmetadata", onMetadata, { once: true });
    video.addEventListener("error", onError, { once: true });
  });
}

async function measureVideo(video, durationMs = 5_000) {
  const startedAt = performance.now();
  let renderedFrames = 0;
  let callbackId;
  const callback = () => {
    renderedFrames += 1;
    if (performance.now() - startedAt < durationMs) {
      callbackId = video.requestVideoFrameCallback(callback);
    }
  };
  if (video.requestVideoFrameCallback) callbackId = video.requestVideoFrameCallback(callback);

  await new Promise((resolve) => window.setTimeout(resolve, durationMs));
  if (callbackId !== undefined && video.cancelVideoFrameCallback) video.cancelVideoFrameCallback(callbackId);

  const quality = video.getVideoPlaybackQuality?.();
  return {
    durationMs,
    renderedFrames,
    droppedFrames: quality?.droppedVideoFrames ?? null,
    totalFrames: quality?.totalVideoFrames ?? null,
    currentTime: Number.isFinite(video.currentTime) ? video.currentTime : null,
  };
}

function byId(id) {
  return document.getElementById(id);
}

function setValue(id, value) {
  const element = byId(id);
  if (element) element.value = value ?? "";
}

function selectedPreset() {
  return CODEC_PRESETS.find((preset) => preset.id === byId("preset")?.value) ?? CODEC_PRESETS[0];
}

function applyPreset(preset) {
  setValue("mime", preset.mime);
  setValue("codec", preset.codec);
  setValue("width", preset.width);
  setValue("height", preset.height);
  setValue("bitrate", preset.bitrate);
  setValue("framerate", preset.framerate);
}

async function runProbe() {
  const source = byId("source").value.trim();
  const mime = byId("mime").value.trim();
  const input = {
    codec: byId("codec").value,
    width: byId("width").value,
    height: byId("height").value,
    bitrate: byId("bitrate").value,
    framerate: byId("framerate").value,
  };
  const video = byId("video");
  const result = { sourceProvided: Boolean(source), mime, input };
  if (!source) throw new Error("请填写本地媒体文件 URL 或 Lux stream URL");
  if (!mime) throw new Error("请填写 MIME 类型");

  result.native = { canPlayType: normalizeNativeSupport(video.canPlayType(mime)) };
  result.mediaCapabilities = await probeMediaCapabilities({ mime, ...input, width: Number(input.width), height: Number(input.height), bitrate: Number(input.bitrate), framerate: Number(input.framerate) });
  result.webCodecs = await probeWebCodecs(input);

  video.src = source;
  video.load();
  result.mediaElement = await waitForVideoMetadata(video);
  if (result.mediaElement.ok) {
    video.muted = true;
    try {
      await video.play();
      result.playback = await measureVideo(video);
      video.pause();
    } catch (error) {
      result.playback = { ok: false, error: error instanceof Error ? error.message : String(error) };
    }
  }
  return result;
}

if (typeof document !== "undefined") {
  const presetSelect = byId("preset");
  for (const preset of CODEC_PRESETS) {
    const option = document.createElement("option");
    option.value = preset.id;
    option.textContent = preset.label;
    presetSelect.append(option);
  }
  presetSelect.value = CODEC_PRESETS[0].id;
  applyPreset(CODEC_PRESETS[0]);
  presetSelect.addEventListener("change", () => applyPreset(selectedPreset()));

  byId("probe-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const output = byId("result");
    const button = byId("run");
    button.disabled = true;
    output.textContent = "正在测试……";
    try {
      output.textContent = JSON.stringify(await runProbe(), null, 2);
    } catch (error) {
      output.textContent = JSON.stringify({ error: error instanceof Error ? error.message : String(error) }, null, 2);
    } finally {
      button.disabled = false;
    }
  });
}
