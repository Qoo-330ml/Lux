export type PlayerFailureKind =
  | "BROWSER_UNSUPPORTED"
  | "PLAYBACK_EXPIRED"
  | "ENGINE_FAILED"
  | "SERVER_PLAN_FAILED";

export type PlayerFailure = {
  kind: PlayerFailureKind;
  title: string;
  message: string;
};

const PLAYER_FAILURES: Record<PlayerFailureKind, PlayerFailure> = {
  BROWSER_UNSUPPORTED: {
    kind: "BROWSER_UNSUPPORTED",
    title: "浏览器不支持此媒体",
    message: "请尝试其他版本、使用支持该格式的浏览器，或改用原生客户端。",
  },
  PLAYBACK_EXPIRED: {
    kind: "PLAYBACK_EXPIRED",
    title: "播放地址已过期",
    message: "请重试以创建新的 Lux 播放会话；若仍失败，请返回详情页重新选择版本。",
  },
  ENGINE_FAILED: {
    kind: "ENGINE_FAILED",
    title: "播放器引擎失败",
    message: "Lux 已停止当前引擎。请重试、选择其他版本，或使用原生客户端。",
  },
  SERVER_PLAN_FAILED: {
    kind: "SERVER_PLAN_FAILED",
    title: "服务端播放计划失败",
    message: "Lux 未能创建可用的播放计划。请重试；若仍失败，请选择其他版本或检查服务器状态。",
  },
};

export function playerFailure(kind: PlayerFailureKind): PlayerFailure {
  return PLAYER_FAILURES[kind];
}

export function classifyPlayerEngineFailure(reason: unknown, status?: number): PlayerFailure {
  const text = reason instanceof Error ? reason.message : typeof reason === "string" ? reason : "";
  if (
    status === 401
    || status === 403
    || status === 410
    || /(?:expired|expiration|过期|签名失效|signature.*(?:invalid|expired))/iu.test(text)
  ) {
    return playerFailure("PLAYBACK_EXPIRED");
  }
  return playerFailure("ENGINE_FAILED");
}
