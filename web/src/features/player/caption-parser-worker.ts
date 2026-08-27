import {
  CaptionParseError,
  parseCaptionText,
  type CaptionFormat,
  type LuxCaptionCue,
} from "./caption-parser";

export type CaptionWorkerRequest = {
  type: "PARSE";
  requestId: number;
  format: CaptionFormat;
  text: string;
};

export type CaptionWorkerResponse =
  | { type: "PARSED"; requestId: number; cues: LuxCaptionCue[] }
  | { type: "FAILED"; requestId: number; message: string };

export function parseCaptionWorkerRequest(request: CaptionWorkerRequest): CaptionWorkerResponse {
  try {
    return {
      type: "PARSED",
      requestId: request.requestId,
      cues: parseCaptionText(request.text, request.format),
    };
  } catch (error) {
    return {
      type: "FAILED",
      requestId: request.requestId,
      message: error instanceof CaptionParseError ? error.message : "字幕解析失败",
    };
  }
}

const workerScope = globalThis as typeof globalThis & {
  onmessage?: (event: MessageEvent<CaptionWorkerRequest>) => void;
  postMessage?: (message: CaptionWorkerResponse) => void;
};

const isWorkerContext = typeof self !== "undefined" && "importScripts" in self;

if (isWorkerContext) {
  workerScope.onmessage = (event) => {
    workerScope.postMessage?.(parseCaptionWorkerRequest(event.data));
  };
}
