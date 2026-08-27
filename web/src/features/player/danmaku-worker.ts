import {
  DanmakuParseError,
  parseBilibiliDanmaku,
  type LuxDanmakuEntry,
} from "./danmaku";

export type DanmakuWorkerRequest = {
  type: "PARSE";
  requestId: number;
  xml: string;
};

export type DanmakuWorkerResponse =
  | { type: "PARSED"; requestId: number; entries: LuxDanmakuEntry[] }
  | { type: "FAILED"; requestId: number; message: string };

export function parseDanmakuWorkerRequest(request: DanmakuWorkerRequest): DanmakuWorkerResponse {
  try {
    return {
      type: "PARSED",
      requestId: request.requestId,
      entries: parseBilibiliDanmaku(request.xml),
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
