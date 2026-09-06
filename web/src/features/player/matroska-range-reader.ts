export type MatroskaRangeReaderOptions = {
  initialBytes?: number;
  maxRangeBytes?: number;
  credentials?: RequestCredentials;
};

export type MatroskaRangeResponse = {
  start: number;
  end: number;
  total: number;
  etag: string | null;
};

export class MatroskaRangeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "MatroskaRangeError";
  }
}

export class MatroskaRangeReader {
  private readonly initialBytes: number;
  private readonly maxRangeBytes: number;
  private readonly credentials: RequestCredentials;

  constructor(
    private readonly source: string,
    options: MatroskaRangeReaderOptions = {},
  ) {
    this.initialBytes = options.initialBytes ?? 1_048_576;
    this.maxRangeBytes = options.maxRangeBytes ?? 32 * 1024 * 1024;
    this.credentials = options.credentials ?? "same-origin";
    if (!Number.isSafeInteger(this.initialBytes) || this.initialBytes <= 0 || !Number.isSafeInteger(this.maxRangeBytes) || this.maxRangeBytes < this.initialBytes) {
      throw new MatroskaRangeError("远程媒体 Range 上限无效");
    }
  }

  async *chunks(signal?: AbortSignal): AsyncGenerator<{ data: Uint8Array; range: MatroskaRangeResponse }, void, void> {
    let start = 0;
    let total: number | null = null;
    let etag: string | null = null;
    let first = true;
    while (total === null || start < total) {
      const requestedEnd = Math.min(
        (total ?? Number.MAX_SAFE_INTEGER) - 1,
        start + (first ? this.initialBytes : this.maxRangeBytes) - 1,
      );
      const result = await this.fetchRange(start, requestedEnd, signal);
      if (total === null) total = result.range.total;
      if (result.range.total !== total) throw new MatroskaRangeError("远程媒体长度发生变化");
      if (etag !== null && result.range.etag !== etag) throw new MatroskaRangeError("远程媒体 ETag 发生变化");
      etag ??= result.range.etag;
      yield result;
      start = result.range.end + 1;
      first = false;
      if (result.range.end < result.range.start || start > total) throw new MatroskaRangeError("远程媒体 Range 边界无效");
    }
  }

  private async fetchRange(start: number, requestedEnd: number, signal?: AbortSignal) {
    const response = await fetch(this.source, {
      credentials: this.credentials,
      mode: "cors",
      headers: { Range: `bytes=${start}-${requestedEnd}` },
      signal,
    });
    if (!response.ok || response.status !== 206) throw new MatroskaRangeError(`客户端媒体读取失败：HTTP ${response.status}`);
    const range = parseContentRange(response.headers.get("content-range"));
    if (!range || range.start !== start || range.end < start || range.end > requestedEnd || !response.body) {
      throw new MatroskaRangeError("客户端媒体读取失败：远程资源未提供有效 Range");
    }
    const chunks: Uint8Array[] = [];
    let bytes = 0;
    const reader = response.body.getReader();
    try {
      while (true) {
        const next = await reader.read();
        if (next.done) break;
        bytes += next.value.byteLength;
        chunks.push(next.value);
      }
    } finally {
      reader.releaseLock();
    }
    if (bytes !== range.end - range.start + 1) throw new MatroskaRangeError("客户端媒体读取失败：Range 长度不一致");
    const data = new Uint8Array(bytes);
    let offset = 0;
    for (const chunk of chunks) { data.set(chunk, offset); offset += chunk.byteLength; }
    return { data, range: { ...range, etag: response.headers.get("etag") } };
  }
}

export function parseContentRange(value: string | null): Omit<MatroskaRangeResponse, "etag"> | null {
  const match = value?.match(/^bytes\s+(\d+)-(\d+)\/(\d+)$/i);
  if (!match) return null;
  const start = Number(match[1]);
  const end = Number(match[2]);
  const total = Number(match[3]);
  if (![start, end, total].every(Number.isSafeInteger) || start < 0 || end < start || total <= end) return null;
  return { start, end, total };
}
