import {
  DanmakuParseError,
  parseDanmakuXml,
  type LuxDanmakuEntry,
} from "./danmaku/parse";

export type DanmakuWorkerRequest = {
  type: "PARSE";
  requestId: number;
  xml: string;
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
    const result = parseDanmakuXml(request.xml);
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
      message: error instanceof DanmakuParseError ? error.message : "弹幕解析失败",
    };
  }
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
