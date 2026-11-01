/**
 * Unified user-facing copy for every playback state the browsers cannot resolve on their own.
 *
 * The player has three surfaces for these states (`docs/API.md`, Web 播放会话):
 * - a blocking layer that replaces the video surface (an `UNSUPPORTED` plan or a rejected session),
 * - a non-blocking banner for a server transcoding session (including automatic degradation),
 * - the plain `DIRECT` plan, which needs no notice at all.
 *
 * This module stays pure so both the player page and its tests can map one input tuple to one
 * notice without rendering anything.
 */

export type PlaybackNoticeSeverity = "error" | "warning" | "info";

export type PlaybackNotice = {
  /** Stable identifier: it keys the dismissal state of a banner. */
  key: string;
  severity: PlaybackNoticeSeverity;
  /** Blocking notices replace the video surface; banners sit above it and never take over input. */
  blocking: boolean;
  title: string;
  message: string;
  /** What the viewer can do about it. Blocking notices always carry one. */
  hint?: string;
};

export type PlaybackNoticeInput = {
  /** `plan.type` of the Web playback session; `null`/`undefined` while it is still loading. */
  planType?: string | null;
  /** `plan.reason` of an `UNSUPPORTED` plan (`STRM_REQUIRES_DIRECT_PLAY`, ...). */
  reason?: string | null;
  /** `session.tier`: 0 direct, 1 remux, 2 audio transcode, 3 hardware, 4 software. */
  tier?: number | null;
  /** HTTP status of a failed session/bootstrap request. */
  status?: number | null;
  /** Stable API error code of that failure (`PERMISSION_DENIED`, `INVALID_REQUEST`, ...). */
  errorCode?: string | null;
  /** True when the client already fell back from Direct Play to a server-side plan. */
  degraded?: boolean | null;
};

type NoticeCopy = Omit<PlaybackNotice, "blocking">;

const EXPIRED: NoticeCopy = {
  key: "playback-expired",
  severity: "warning",
  title: "播放会话已失效",
  message: "登录状态、播放会话或签名地址已经失效，服务端拒绝了这次播放；媒体条目也可能已被移除，或当前账号无权访问。",
  hint: "重试会重新创建播放会话；如果反复出现，请重新登录或返回详情页重新开始播放。",
};

const POLICY_DENIED: NoticeCopy = {
  key: "playback-policy-denied",
  severity: "error",
  title: "当前账号无权使用所需转码",
  message: "你的播放策略没有放行这个媒体需要的服务端转码档位，Lux 因此没有创建播放会话。",
  hint: "请联系管理员调整你的播放策略；在策略调整前，可以改用原生客户端，或选择不需要转码的版本。",
};

const TRANSCODE_CAPACITY: NoticeCopy = {
  key: "transcode-capacity",
  severity: "warning",
  title: "服务端转码暂时不可用",
  message: "服务器当前的转码并发已经占满，或本次转码启动失败（HTTP 502），播放没有开始。",
  hint: "稍后重试；如果同时有多路播放，先停止其中一路，或改用原生客户端。",
};

const PLAYBACK_UNAVAILABLE: NoticeCopy = {
  key: "playback-unavailable",
  severity: "warning",
  title: "播放服务暂时不可用",
  message: "Lux 暂时无法校验播放授权或读取播放会话数据（HTTP 503）。",
  hint: "请稍后重试；如果持续失败，请联系管理员检查服务状态。",
};

const REQUEST_STALE: NoticeCopy = {
  key: "playback-request-stale",
  severity: "warning",
  title: "请求状态已失效",
  message: "这次播放请求没有通过服务端的会话校验（例如页面长时间未操作、跨标签页操作或登录状态已变化），服务端因此拒绝了它。",
  hint: "刷新页面或重新登录后再试；如果仍然失败，请返回详情页重新开始播放。",
};

const REQUEST_INVALID: NoticeCopy = {
  key: "playback-request-invalid",
  severity: "error",
  title: "播放请求被拒绝",
  message: "服务端认为这次播放请求的参数不合法（例如画质上限超出允许范围），因此没有创建播放会话。",
  hint: "返回详情页重新选择版本或画质后再试。",
};

const STRM_REQUIRES_DIRECT_PLAY: NoticeCopy = {
  key: "strm-requires-direct-play",
  severity: "error",
  title: "STRM 源只能直连播放",
  message: ".strm 远程源在 Lux 中只支持直连播放，不代理媒体字节、也不交给服务端转码，而当前浏览器无法直接播放它的编码或容器格式。",
  hint: "尝试选择其他版本；改用原生客户端（Infuse、VidHub 等）播放，或请管理员换一个浏览器可以直接播放的版本。",
};

const BROWSER_CANNOT_CONSUME_HLS: NoticeCopy = {
  key: "browser-cannot-consume-hls",
  severity: "error",
  title: "当前浏览器无法播放服务端 HLS",
  message: "浏览器没有声明支持 HLS/MSE，服务端因此无法用分片转码的方式输出这段媒体。",
  hint: "尝试选择其他版本，或换用支持 MSE 的现代浏览器（新版 Chrome、Edge、Firefox 或 Safari）、原生客户端。",
};

const NO_COMPATIBLE_SERVER_PLAN: NoticeCopy = {
  key: "no-compatible-server-plan",
  severity: "error",
  title: "没有可用的服务端转码档位",
  message: "Lux 找不到既能输出浏览器可解码的格式、又符合当前画质上限与并发配置的转码档位。",
  hint: "调高画质上限或选择其他版本再试；如果仍然失败，请联系管理员检查转码设置（硬件编码器是否可用、是否允许软件转码）。",
};

const SERVER_TRANSCODING_DISABLED: NoticeCopy = {
  key: "server-transcoding-disabled",
  severity: "error",
  title: "服务端转码已被关闭",
  message: "管理员关闭了实时转码，只有浏览器能直接解码的媒体才能在网页端播放。",
  hint: "请联系管理员开启实时转码，或改用原生客户端播放。",
};

/** A `Map` keeps prototype keys (`__proto__`, `constructor`) from resolving to copy objects. */
const UNSUPPORTED_BY_REASON = new Map<string, NoticeCopy>([
  ["STRM_REQUIRES_DIRECT_PLAY", STRM_REQUIRES_DIRECT_PLAY],
  ["BROWSER_CANNOT_CONSUME_HLS", BROWSER_CANNOT_CONSUME_HLS],
  ["NO_COMPATIBLE_SERVER_PLAN", NO_COMPATIBLE_SERVER_PLAN],
  ["SERVER_TRANSCODING_DISABLED", SERVER_TRANSCODING_DISABLED],
]);

type TranscodeCopy = { title: string; message: string; hint?: string };

/** Banner copy per server tier (`docs/API.md` 档位 0–4). */
const TRANSCODE_BY_TIER: Record<number, TranscodeCopy> = {
  1: {
    title: "服务端正在做封装转换",
    message: "服务器只把原始码流重新封装成 HLS，视频和音频都不重新编码，起播需要先缓冲一小段。",
  },
  2: {
    title: "服务端正在转码音轨",
    message: "服务器把音轨转成浏览器可解码的格式，视频保持原始码流。",
  },
  3: {
    title: "服务端正在硬件转码",
    message: "服务器用硬件编码器实时转码视频，画面需要少量缓冲；硬件不可用或启动失败时，Lux 会自动改用软件转码。",
  },
  4: {
    title: "服务端正在软件转码",
    message: "服务器用 CPU 实时转码视频，起播和拖动后都需要等待缓冲，服务器繁忙时可能卡顿。",
    hint: "如果频繁卡顿，可以降低画质上限，或改用原生客户端。",
  },
};

/**
 * Maps the playback plan and any session failure to at most one notice.
 *
 * A rejected session wins over the plan because the request never produced a usable plan; for
 * `SERVER_HLS` the banner describes whichever transcode tier the server actually runs.
 */
export function playbackNoticeFor(input: PlaybackNoticeInput): PlaybackNotice | null {
  const failure = playbackFailureNotice(input);
  if (failure) return failure;
  if (input.planType === "UNSUPPORTED") return unsupportedNotice(input.reason);
  if (input.planType === "SERVER_HLS") return transcodeNotice(input.tier, input.degraded === true);
  return null;
}

/** Single-paragraph rendering for hosts that own their own layout (`PlayerErrorState`). */
export function playbackNoticeSummary(notice: PlaybackNotice): string {
  return notice.hint ? `${notice.message} ${notice.hint}` : notice.message;
}

function playbackFailureNotice({ status, errorCode }: PlaybackNoticeInput): PlaybackNotice | null {
  const code = (errorCode ?? "").toUpperCase();
  // A stale CSRF/session check is not a transcode policy denial: the plan may be perfectly legal.
  if (code === "CSRF_FAILED") return blocking(REQUEST_STALE);
  if (code === "PERMISSION_DENIED" || (status === 403 && !code)) return blocking(POLICY_DENIED);
  if (status === 403) return blocking(REQUEST_STALE);
  if (status === 502) return blocking(TRANSCODE_CAPACITY);
  if (code === "DATABASE_UNAVAILABLE" || status === 503) return blocking(PLAYBACK_UNAVAILABLE);
  if (code === "INVALID_REQUEST" || status === 400) return blocking(REQUEST_INVALID);
  if (code === "NOT_FOUND" || status === 401 || status === 404 || status === 410) {
    return blocking(EXPIRED);
  }
  return null;
}

function unsupportedNotice(reason: string | null | undefined): PlaybackNotice {
  const code = typeof reason === "string" ? reason.trim() : "";
  const known = code ? UNSUPPORTED_BY_REASON.get(code) : undefined;
  if (known) return blocking(known);
  return blocking({
    key: "unsupported-unknown",
    severity: "warning",
    title: "这个媒体暂时无法在网页端播放",
    message: code
      ? `服务端拒绝为该媒体生成播放计划（原因码：${code}），网页端无法回退到其他播放方式。`
      : "服务端拒绝为该媒体生成播放计划，但没有给出原因码，网页端无法回退到其他播放方式。",
    hint: "重试可能恢复；如果一直失败，请选择其他版本、改用原生客户端，或联系管理员。",
  });
}

function transcodeNotice(tier: number | null | undefined, degraded: boolean): PlaybackNotice | null {
  if (typeof tier !== "number" || !Number.isFinite(tier)) return null;
  const copy = TRANSCODE_BY_TIER[tier];
  if (!copy) return null;
  // A plain remux is visually transparent, so it only deserves a banner when it was a fallback.
  if (tier === 1 && !degraded) return null;
  const key = degraded ? `server-hls-degraded-tier-${tier}` : `server-hls-tier-${tier}`;
  return {
    key,
    severity: "info",
    blocking: false,
    title: copy.title,
    message: degraded
      ? `浏览器无法直接播放原始码流，Lux 已自动改用服务端 HLS 输出。${copy.message}`
      : copy.message,
    ...(degraded
      ? { hint: copy.hint ?? "如果画面卡顿或等待过久，可以暂停后等待缓冲，或改用原生客户端。" }
      : copy.hint
        ? { hint: copy.hint }
        : {}),
  };
}

function blocking(copy: NoticeCopy): PlaybackNotice {
  return { ...copy, blocking: true };
}
