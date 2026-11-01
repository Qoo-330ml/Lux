import { describe, expect, it } from "vitest";
import {
  playbackNoticeFor,
  playbackNoticeSummary,
  type PlaybackNotice,
} from "../src/features/player/playback-notices";

function notice(input: Parameters<typeof playbackNoticeFor>[0]): PlaybackNotice {
  const result = playbackNoticeFor(input);
  if (!result) throw new Error(`expected a notice for ${JSON.stringify(input)}`);
  return result;
}

describe("playbackNoticeFor: UNSUPPORTED reasons", () => {
  it("explains that a .strm source can only be played directly", () => {
    const result = notice({ planType: "UNSUPPORTED", reason: "STRM_REQUIRES_DIRECT_PLAY" });

    expect(result).toMatchObject({
      key: "strm-requires-direct-play",
      severity: "error",
      blocking: true,
      title: "STRM 源只能直连播放",
    });
    expect(result.message).toContain("直连播放");
    expect(result.hint).toContain("原生客户端");
  });

  it("asks the viewer to switch browsers when HLS cannot be consumed", () => {
    const result = notice({ planType: "UNSUPPORTED", reason: "BROWSER_CANNOT_CONSUME_HLS" });

    expect(result).toMatchObject({
      key: "browser-cannot-consume-hls",
      severity: "error",
      blocking: true,
    });
    expect(result.message).toContain("HLS");
    expect(result.hint).toContain("浏览器");
  });

  it("names the missing transcode tier and points at the administrator", () => {
    const result = notice({ planType: "UNSUPPORTED", reason: "NO_COMPATIBLE_SERVER_PLAN" });

    expect(result).toMatchObject({
      key: "no-compatible-server-plan",
      severity: "error",
      blocking: true,
      title: "没有可用的服务端转码档位",
    });
    expect(result.hint).toContain("管理员");
    expect(result.hint).toContain("画质上限");
  });

  it("explains that an administrator turned server transcoding off", () => {
    const result = notice({ planType: "UNSUPPORTED", reason: "SERVER_TRANSCODING_DISABLED" });

    expect(result).toMatchObject({
      key: "server-transcoding-disabled",
      severity: "error",
      blocking: true,
    });
    expect(result.message).toContain("实时转码");
    expect(result.hint).toContain("管理员");
  });

  it("keeps the reason code for an unknown UNSUPPORTED reason", () => {
    const result = notice({ planType: "UNSUPPORTED", reason: "FUTURE_REASON" });

    expect(result).toMatchObject({ key: "unsupported-unknown", severity: "warning", blocking: true });
    expect(result.message).toContain("FUTURE_REASON");
    expect(result.hint).toContain("其他版本");
  });

  it("does not invent a reason code when the server omitted it", () => {
    const result = notice({ planType: "UNSUPPORTED", reason: null });

    expect(result.key).toBe("unsupported-unknown");
    expect(result.message).toContain("没有给出原因码");
    expect(result.message).not.toContain("null");
  });

  it("ignores prototype keys supplied as an UNSUPPORTED reason", () => {
    for (const reason of ["__proto__", "constructor", "toString", "hasOwnProperty"]) {
      const result = notice({ planType: "UNSUPPORTED", reason });

      expect(result.key).toBe("unsupported-unknown");
      expect(result.title).toBe("这个媒体暂时无法在网页端播放");
      expect(result.message).toContain(reason);
      expect(result.hint).toBeTruthy();
    }
  });

  it("suggests another version for the STRM and browser-capability reasons", () => {
    expect(notice({ planType: "UNSUPPORTED", reason: "STRM_REQUIRES_DIRECT_PLAY" }).hint)
      .toContain("其他版本");
    expect(notice({ planType: "UNSUPPORTED", reason: "BROWSER_CANNOT_CONSUME_HLS" }).hint)
      .toContain("其他版本");
  });
});

describe("playbackNoticeFor: session failures", () => {
  it("explains a playback policy denial and who can change it", () => {
    const byStatus = notice({ status: 403 });
    const byCode = notice({ status: 403, errorCode: "PERMISSION_DENIED" });

    expect(byStatus).toMatchObject({
      key: "playback-policy-denied",
      severity: "error",
      blocking: true,
    });
    expect(byCode.key).toBe(byStatus.key);
    expect(byStatus.message).toContain("播放策略");
    expect(byStatus.hint).toContain("管理员");
  });

  it("reads a CSRF_FAILED 403 as a stale request state instead of a policy denial", () => {
    const result = notice({ status: 403, errorCode: "CSRF_FAILED" });

    expect(result).toMatchObject({
      key: "playback-request-stale",
      severity: "warning",
      blocking: true,
    });
    expect(result.title).toContain("已失效");
    expect(result.message).toContain("会话校验");
    expect(result.hint).toContain("刷新页面");
    expect(result.hint).toContain("重新登录");
    expect(result.message).not.toContain("转码");
    expect(result.hint).not.toContain("管理员");
  });

  it("explains a 502 as exhausted server transcode capacity", () => {
    const result = notice({ status: 502 });

    expect(result).toMatchObject({ key: "transcode-capacity", severity: "warning", blocking: true });
    expect(result.message).toContain("转码并发");
    expect(result.message).toContain("502");
    expect(result.hint).toContain("稍后重试");
  });

  it("explains a 503 as a temporarily unavailable playback service", () => {
    const result = notice({ status: 503, errorCode: "DATABASE_UNAVAILABLE" });

    expect(result).toMatchObject({ key: "playback-unavailable", severity: "warning", blocking: true });
    expect(result.hint).toContain("稍后重试");
  });

  it("explains a rejected request without blaming the media", () => {
    const result = notice({ status: 400, errorCode: "INVALID_REQUEST" });

    expect(result).toMatchObject({ key: "playback-request-invalid", severity: "error", blocking: true });
    expect(result.hint).toContain("重新选择");
  });

  it("explains expired or missing sessions", () => {
    for (const status of [401, 404, 410]) {
      const result = notice({ status });
      expect(result.key).toBe("playback-expired");
      expect(result.blocking).toBe(true);
      expect(result.hint).toContain("重试");
    }
    expect(notice({ status: 404, errorCode: "NOT_FOUND" }).key).toBe("playback-expired");
  });

  it("prefers the failure notice over the UNSUPPORTED reason", () => {
    const result = notice({
      planType: "UNSUPPORTED",
      reason: "NO_COMPATIBLE_SERVER_PLAN",
      status: 403,
      errorCode: "PERMISSION_DENIED",
    });

    expect(result.key).toBe("playback-policy-denied");
  });

  it("ignores failures that carry no usable signal", () => {
    expect(playbackNoticeFor({ status: 0 })).toBeNull();
    expect(playbackNoticeFor({ status: null, errorCode: "UNKNOWN" })).toBeNull();
  });
});

describe("playbackNoticeFor: server transcode banners", () => {
  it("describes hardware transcoding in a non-blocking banner", () => {
    const result = notice({ planType: "SERVER_HLS", tier: 3 });

    expect(result).toMatchObject({
      key: "server-hls-tier-3",
      severity: "info",
      blocking: false,
      title: "服务端正在硬件转码",
    });
  });

  it("warns that software transcoding may stutter and suggests lowering quality", () => {
    const result = notice({ planType: "SERVER_HLS", tier: 4 });

    expect(result).toMatchObject({
      key: "server-hls-tier-4",
      severity: "info",
      blocking: false,
      title: "服务端正在软件转码",
    });
    expect(result.hint).toContain("画质");
  });

  it("describes audio transcoding and remux tiers", () => {
    expect(notice({ planType: "SERVER_HLS", tier: 2 }).title).toContain("音轨");
    // A pure remux is transparent enough to stay silent unless it was a fallback.
    expect(playbackNoticeFor({ planType: "SERVER_HLS", tier: 1 })).toBeNull();
  });

  it("marks the banner as a client fallback when direct play already failed", () => {
    const result = notice({ planType: "SERVER_HLS", tier: 4, degraded: true });

    expect(result).toMatchObject({ key: "server-hls-degraded-tier-4", blocking: false });
    expect(result.message).toContain("浏览器无法直接播放原始码流");
  });

  it("shows the remux banner when it is already a fallback", () => {
    const result = notice({ planType: "SERVER_HLS", tier: 1, degraded: true });

    expect(result.key).toBe("server-hls-degraded-tier-1");
    expect(result.title).toContain("封装转换");
  });

  it("stays silent for direct play, loading sessions and unknown tiers", () => {
    expect(playbackNoticeFor({ planType: "DIRECT", tier: 0 })).toBeNull();
    expect(playbackNoticeFor({ planType: null, tier: null })).toBeNull();
    expect(playbackNoticeFor({ planType: "SERVER_HLS", tier: null })).toBeNull();
    expect(playbackNoticeFor({ planType: "SERVER_HLS", tier: 0 })).toBeNull();
    expect(playbackNoticeFor({ planType: "SOMETHING_NEW" })).toBeNull();
  });
});

describe("playbackNoticeFor: copy invariants", () => {
  const samples: Parameters<typeof playbackNoticeFor>[0][] = [
    { planType: "UNSUPPORTED", reason: "STRM_REQUIRES_DIRECT_PLAY" },
    { planType: "UNSUPPORTED", reason: "BROWSER_CANNOT_CONSUME_HLS" },
    { planType: "UNSUPPORTED", reason: "NO_COMPATIBLE_SERVER_PLAN" },
    { planType: "UNSUPPORTED", reason: "SERVER_TRANSCODING_DISABLED" },
    { planType: "UNSUPPORTED", reason: "UNKNOWN_CODE" },
    { status: 403 },
    { status: 502 },
    { status: 503 },
    { status: 400 },
    { status: 410 },
    { planType: "SERVER_HLS", tier: 2 },
    { planType: "SERVER_HLS", tier: 3 },
    { planType: "SERVER_HLS", tier: 4 },
    { planType: "SERVER_HLS", tier: 4, degraded: true },
  ];

  it("always gives a Chinese title, a message and a stable unique key", () => {
    const keys = samples.map((sample) => notice(sample).key);

    expect(new Set(keys).size).toBe(keys.length);
    for (const sample of samples) {
      const result = notice(sample);
      expect(result.title.length).toBeGreaterThan(0);
      expect(result.message.length).toBeGreaterThan(0);
      expect(result.title).toMatch(/[\u4e00-\u9fa5]/u);
      expect(result.message).toMatch(/[\u4e00-\u9fa5]/u);
    }
  });

  it("gives every blocking notice an actionable hint", () => {
    for (const sample of samples) {
      const result = notice(sample);
      if (!result.blocking) continue;
      expect(result.hint, result.key).toBeTruthy();
    }
  });

  it("joins message and hint for single-paragraph hosts", () => {
    const result = notice({ status: 403 });

    expect(playbackNoticeSummary(result)).toBe(`${result.message} ${result.hint}`);
    expect(playbackNoticeSummary({ ...result, hint: undefined })).toBe(result.message);
  });
});
