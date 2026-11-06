/**
 * Wire types and validation for `GET /api/v1/ws` (ADR-054 "协议与信封").
 *
 * The server sends exactly two downlink frames and never any business data:
 * a one-shot `ready` and idempotent `invalidate` notifications. The client
 * sends nothing but the application-level `ping`.
 */

export type RealtimeConnectionState = "connected" | "disconnected";

export type RealtimeChannel = "user" | "admin";

export type ReadyFrame = {
  type: "ready";
  version: number;
};

export type InvalidateFrame = {
  type: "invalidate";
  channel: RealtimeChannel;
  scope: string;
  itemId?: string;
};

export type RealtimeFrame = ReadyFrame | InvalidateFrame;

/** ADR-054: an `itemId` longer than this makes the whole frame invalid. */
export const REALTIME_ITEM_ID_MAX_LENGTH = 256;

const CHANNELS: readonly string[] = ["user", "admin"];

/**
 * Parses one downlink frame.
 *
 * Unknown fields are ignored (the envelope is additive), but a frame that
 * carries a `channel` this client cannot attribute, a non-string `scope`, or an
 * unusable `itemId` is dropped as a whole instead of being half-applied.
 */
export function parseRealtimeFrame(data: unknown): RealtimeFrame | null {
  if (typeof data !== "string" || data.length === 0) return null;
  let payload: unknown;
  try {
    payload = JSON.parse(data);
  } catch {
    return null;
  }
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) return null;
  const frame = payload as Record<string, unknown>;

  if (frame.type === "ready") {
    return { type: "ready", version: typeof frame.version === "number" ? frame.version : 0 };
  }
  if (frame.type !== "invalidate") return null;

  if (typeof frame.channel !== "string" || !CHANNELS.includes(frame.channel)) return null;
  if (typeof frame.scope !== "string" || frame.scope.trim().length === 0) return null;

  let itemId: string | undefined;
  if ("itemId" in frame) {
    const value = frame.itemId;
    if (typeof value !== "string") return null;
    if (value.trim().length === 0 || value.length > REALTIME_ITEM_ID_MAX_LENGTH) return null;
    itemId = value;
  }

  return { type: "invalidate", channel: frame.channel as RealtimeChannel, scope: frame.scope, itemId };
}
