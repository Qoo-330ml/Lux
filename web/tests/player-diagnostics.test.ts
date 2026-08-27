import { describe, expect, it } from "vitest";
import {
  classifyPlayerEngineFailure,
  playerFailure,
} from "../src/features/player/components/player-diagnostics";

describe("LuxPlayer playback diagnostics", () => {
  it("gives browser, expiry, engine, and server-plan failures distinct Lux guidance", () => {
    expect(playerFailure("BROWSER_UNSUPPORTED")).toMatchObject({
      kind: "BROWSER_UNSUPPORTED",
      title: "浏览器不支持此媒体",
    });
    expect(playerFailure("PLAYBACK_EXPIRED")).toMatchObject({
      kind: "PLAYBACK_EXPIRED",
      title: "播放地址已过期",
    });
    expect(playerFailure("ENGINE_FAILED")).toMatchObject({
      kind: "ENGINE_FAILED",
      title: "播放器引擎失败",
    });
    expect(playerFailure("SERVER_PLAN_FAILED")).toMatchObject({
      kind: "SERVER_PLAN_FAILED",
      title: "服务端播放计划失败",
    });
  });

  it("identifies expired playback failures without exposing the upstream reason", () => {
    const failure = classifyPlayerEngineFailure("signed playback session has expired");

    expect(failure).toEqual(playerFailure("PLAYBACK_EXPIRED"));
    expect(failure.message).not.toContain("signed playback session has expired");
  });

  it("maps unrecognized engine details to safe recovery guidance", () => {
    expect(classifyPlayerEngineFailure("decoder emitted an internal code 42"))
      .toEqual(playerFailure("ENGINE_FAILED"));
  });
});
