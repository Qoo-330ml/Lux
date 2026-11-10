import {
  DanmakuParseError,
  decodeDanmakuBytes,
  parseDanmakuXml,
  type LuxDanmakuEntry,
} from "./danmaku/parse";

export type DanmakuWorkerRequest = {
  type: "PARSE";
  requestId: number;
  /**
   * The raw response bytes, posted as a transferable so the main thread neither decodes the XML nor
   * copies a large string across the worker boundary.
   */
  bytes: ArrayBuffer;
};

export type DanmakuWorkerResponse =
  | {
    type: "PARSED";
    requestId: number;
    entries: LuxDanmakuEntry[];
    /**
     * A file with more entries than the parse budget is sampled instead of rejected; the page uses
     * this flag to tell the user that what they see is truncated.
     */
    truncated: boolean;
    totalEntries: number;
  }
  | { type: "FAILED"; requestId: number; message: string };

export function parseDanmakuWorkerRequest(request: DanmakuWorkerRequest): DanmakuWorkerResponse {
  try {
    const xml = decodeDanmakuBytes(request.bytes);
    const result = parseDanmakuXml(xml);
    return {
      type: "PARSED",
      requestId: request.requestId,
      entries: result.entries,
      truncated: result.truncated,
      totalEntries: result.totalEntries,
    };
  } catch (error) {
    return {
      type: "FAILED",
      requestId: request.requestId,
      message: workerFailureMessage(error),
    };
  }
}

function workerFailureMessage(error: unknown) {
  if (error instanceof DanmakuParseError) return error.message;
  // A fatal UTF-8 decode failure is actionable for the user; anything else is a generic failure.
  if (error instanceof Error && error.message === "弹幕内容编码无效") return error.message;
  return "弹幕解析失败";
}

const workerScope = globalThis as typeof globalThis & {
  onmessage?: (event: MessageEvent<DanmakuWorkerRequest>) => void;
  postMessage?: (message: DanmakuWorkerResponse) => void;
};

const isWorkerContext = typeof self !== "undefined" && "importScripts" in self;

if (isWorkerContext) {
  workerScope.onmessage = (event) => {
    workerScope.postMessage?.(parseDanmakuWorkerRequest(event.data));
  };
}
