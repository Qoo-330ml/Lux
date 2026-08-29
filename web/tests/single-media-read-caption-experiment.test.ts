import { describe, expect, it, vi } from "vitest";
import {
  evaluateSingleMediaReadCaptionGate,
  runSingleMediaReadCaptionExperiment,
  type SingleMediaReadCaptionContext,
} from "../src/features/player/single-media-read-caption-experiment";

function context(overrides: Partial<SingleMediaReadCaptionContext> = {}): SingleMediaReadCaptionContext {
  return {
    enabled: true,
    sourceKind: "STRM_URL",
    response: new Response(null, {
      status: 206,
      headers: {
        "content-type": "video/x-matroska",
        "accept-ranges": "bytes",
        "content-range": "bytes 0-7/8",
      },
    }),
    body: new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array([1, 2, 3]));
        controller.close();
      },
    }),
    request: { mode: "cors", rangeRequested: true },
    parser: (bytes) => ({ byteCount: bytes.byteLength }),
    ...overrides,
  };
}

describe("single media-read caption experiment", () => {
  it("is disabled by default and does not inspect or consume the body", async () => {
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array([1]));
        controller.close();
      },
    });
    const parser = vi.fn();

    const result = await runSingleMediaReadCaptionExperiment(context({ enabled: false, body, parser }));

    expect(result).toMatchObject({ status: "skipped", reason: "disabled", bytesRead: 0 });
    expect(parser).not.toHaveBeenCalled();
  });

  it("requires the current CORS Range media response and a parser before reading", () => {
    expect(evaluateSingleMediaReadCaptionGate(context({ request: { mode: "no-cors", rangeRequested: true } }))).toMatchObject({
      allowed: false,
      reason: "cors-unavailable",
    });
    expect(evaluateSingleMediaReadCaptionGate(context({ request: { mode: "cors", rangeRequested: false } }))).toMatchObject({
      allowed: false,
      reason: "range-unavailable",
    });
    expect(evaluateSingleMediaReadCaptionGate(context({ response: new Response(null, {
      status: 200,
      headers: { "accept-ranges": "bytes" },
    }) }))).toMatchObject({
      allowed: false,
      reason: "media-type-unavailable",
    });
    expect(evaluateSingleMediaReadCaptionGate(context({ parser: undefined }))).toMatchObject({
      allowed: false,
      reason: "parser-unavailable",
    });
    expect(evaluateSingleMediaReadCaptionGate(context({ body: null }))).toMatchObject({
      allowed: false,
      reason: "body-unavailable",
    });
    expect(evaluateSingleMediaReadCaptionGate(context({ response: new Response(null, {
      status: 200,
      headers: {
        "content-type": "video/x-matroska",
        "accept-ranges": "bytes",
        "content-length": "9000000",
      },
    }) }))).toMatchObject({
      allowed: false,
      reason: "byte-limit-exceeded",
    });
    expect(evaluateSingleMediaReadCaptionGate(context({ response: new Response(null, {
      status: 500,
      headers: {
        "content-type": "video/x-matroska",
        "accept-ranges": "bytes",
      },
    }) }))).toMatchObject({
      allowed: false,
      reason: "range-unavailable",
    });
  });

  it("parses the supplied body branch without making a fetch request", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const result = await runSingleMediaReadCaptionExperiment(context());

    expect(result).toMatchObject({
      status: "parsed",
      bytesRead: 3,
      value: { byteCount: 3 },
      diagnostics: { sourceKind: "STRM_URL", mediaType: "video/x-matroska" },
    });
    expect(fetchSpy).not.toHaveBeenCalled();
    fetchSpy.mockRestore();
  });

  it("stops at the byte limit and cancels the experiment branch", async () => {
    let cancelled = false;
    const body = new ReadableStream<Uint8Array>({
      pull(controller) {
        controller.enqueue(new Uint8Array([1, 2, 3, 4]));
      },
      cancel() {
        cancelled = true;
      },
    });

    const result = await runSingleMediaReadCaptionExperiment(context({
      body,
      maxBytes: 3,
    }));

    expect(result).toMatchObject({ status: "failed", reason: "byte-limit-exceeded", bytesRead: 4 });
    expect(cancelled).toBe(true);
  });

  it("returns cancellation and parser failures without changing the playback result", async () => {
    const controller = new AbortController();
    controller.abort();
    const cancelled = await runSingleMediaReadCaptionExperiment(context({ signal: controller.signal }));
    expect(cancelled).toMatchObject({ status: "failed", reason: "cancelled" });

    const parserFailure = await runSingleMediaReadCaptionExperiment(context({
      parser: () => { throw new Error("malformed caption payload"); },
    }));
    expect(parserFailure).toMatchObject({ status: "failed", reason: "parser-failed" });
  });

  it("cancels a pending read when the playback lifecycle is aborted", async () => {
    const controller = new AbortController();
    let cancelled = false;
    const body = new ReadableStream<Uint8Array>({
      start(streamController) {
        streamController.enqueue(new Uint8Array([1]));
      },
      cancel() {
        cancelled = true;
      },
    });
    setTimeout(() => controller.abort(), 0);

    const result = await runSingleMediaReadCaptionExperiment(context({ body, signal: controller.signal }));

    expect(result).toMatchObject({ status: "failed", reason: "cancelled", bytesRead: 1 });
    expect(cancelled).toBe(true);
  });

  it("returns a timeout result and cancels a body that never produces another chunk", async () => {
    let cancelled = false;
    const body = new ReadableStream<Uint8Array>({
      pull() {
        return new Promise<void>(() => undefined);
      },
      cancel() {
        cancelled = true;
      },
    });

    const result = await runSingleMediaReadCaptionExperiment(context({ body, timeoutMs: 5 }));

    expect(result).toMatchObject({ status: "failed", reason: "timed-out", bytesRead: 0 });
    expect(cancelled).toBe(true);
  });

  it("skips path STRM and opaque responses without reading their media bytes", async () => {
    const reader = vi.fn();
    const body = { getReader: reader } as unknown as ReadableStream<Uint8Array>;
    const pathResult = await runSingleMediaReadCaptionExperiment(context({ sourceKind: "STRM_PATH", body }));
    const opaqueResult = await runSingleMediaReadCaptionExperiment(context({
      response: { status: 0, type: "opaque", headers: new Headers() } as Response,
      body,
    }));

    expect(pathResult).toMatchObject({ status: "skipped", reason: "unsupported-source" });
    expect(opaqueResult).toMatchObject({ status: "skipped", reason: "cors-unavailable" });
    expect(reader).not.toHaveBeenCalled();
  });
});
