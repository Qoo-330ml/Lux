// @vitest-environment jsdom

import { beforeEach, describe, expect, it } from "vitest";
import {
  clearRememberedCandidate,
  directCandidates,
  orderCandidates,
  rememberCandidate,
  rememberedCandidate,
} from "../src/features/player/direct-candidates";
import type { MediaSource, WebPlaybackPlan } from "../src/lib/api/types";

const direct = (extra: Partial<Extract<WebPlaybackPlan, { type: "DIRECT" }>> = {}): WebPlaybackPlan => ({
  type: "DIRECT",
  url: "/api/v1/playback/sessions/s1/direct?expires=1&signature=x",
  ...extra,
});

const remoteHttp = { id: "m1", sourceKind: "STRM_URL", externalUrl: "https://cdn.example.test/v.mp4" } as MediaSource;
const pathStrm = { id: "m2", sourceKind: "STRM_URL", externalUrl: "/mnt/media/v.mp4" } as MediaSource;
const local = { id: "m3", sourceKind: "LOCAL_FILE" } as MediaSource;

describe("directCandidates", () => {
  it("tries the 302 proxy, then the signed Lux entry, then the origin for remote HTTP STRM", () => {
    const list = directCandidates(direct({ proxyUrl: "/Videos/1/stream.mp4?MediaSourceId=m1" }), remoteHttp);
    expect(list.map((entry) => entry.kind)).toEqual(["proxy", "lux", "origin"]);
    expect(list[2].url).toBe("https://cdn.example.test/v.mp4");
  });

  it("uses the server-ordered candidates when the plan carries them", () => {
    const list = directCandidates(
      direct({
        proxyUrl: "/legacy",
        candidates: [
          { kind: "proxy", url: "/Videos/1/stream?api_key=luxpb_1_x" },
          { kind: "lux", url: "/api/v1/playback/sessions/s1/direct" },
          { kind: "origin", url: "https://cdn.example.test/v.mp4" },
          { kind: "unknown", url: "/ignored" },
          { kind: "lux", url: "" },
        ],
      }),
      remoteHttp,
    );
    expect(list.map((entry) => entry.kind)).toEqual(["proxy", "lux", "origin"]);
    expect(list[0].url).toBe("/Videos/1/stream?api_key=luxpb_1_x");
  });

  it("falls back to the legacy fields when the candidate list is empty", () => {
    const list = directCandidates(direct({ candidates: [], proxyUrl: "/Videos/2/stream" }), pathStrm);
    expect(list.map((entry) => entry.kind)).toEqual(["proxy", "lux"]);
  });

  it("keeps the origin as the only candidate when the plan has no proxy URL", () => {
    expect(directCandidates(direct(), remoteHttp)).toEqual([
      { kind: "origin", url: "https://cdn.example.test/v.mp4" },
    ]);
  });

  it("keeps proxy then Lux for path STRM and never adds an origin", () => {
    const list = directCandidates(direct({ proxyUrl: "/Videos/2/stream.mp4?MediaSourceId=m2" }), pathStrm);
    expect(list.map((entry) => entry.kind)).toEqual(["proxy", "lux"]);
  });

  it("uses only the Lux entry for local media", () => {
    expect(directCandidates(direct(), local).map((entry) => entry.kind)).toEqual(["lux"]);
  });

  it("returns nothing for non-direct plans", () => {
    expect(directCandidates({ type: "SERVER_HLS", manifestUrl: "/m.m3u8", tier: 1 }, remoteHttp)).toEqual([]);
    expect(directCandidates(undefined, remoteHttp)).toEqual([]);
  });
});

describe("candidate memory", () => {
  beforeEach(() => window.localStorage.clear());

  const list = directCandidates(direct({ proxyUrl: "/Videos/1/stream.mp4?MediaSourceId=m1" }), remoteHttp);

  it("starts from the remembered kind and keeps the rest in order", () => {
    expect(orderCandidates(list, "lux").map((entry) => entry.kind)).toEqual(["lux", "proxy", "origin"]);
    expect(orderCandidates(list, "origin").map((entry) => entry.kind)).toEqual(["origin", "proxy", "lux"]);
  });

  it("ignores a remembered kind the plan does not offer", () => {
    expect(orderCandidates(list.slice(0, 2), "origin").map((entry) => entry.kind)).toEqual(["proxy", "lux"]);
    expect(orderCandidates(list, null)).toEqual(list);
  });

  it("stores only the kind, never a URL", () => {
    rememberCandidate("lux");
    expect(rememberedCandidate()).toBe("lux");
    expect(Object.values(window.localStorage).join("")).not.toMatch(/https?:|\//);
  });

  it("drops unknown stored values and can be cleared", () => {
    window.localStorage.setItem("lux.player.directCandidate", "https://evil.example.test");
    expect(rememberedCandidate()).toBeNull();
    rememberCandidate("proxy");
    clearRememberedCandidate();
    expect(rememberedCandidate()).toBeNull();
  });
});
