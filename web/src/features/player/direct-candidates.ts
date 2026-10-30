import type { MediaSource, WebPlaybackPlan } from "../../lib/api/types";
import { isRemoteHttpStrmSource } from "./playback-selection";

/**
 * Where a Direct Play request is sent. `proxy` is the same-origin Emby
 * `/Videos/{id}/stream` entry that a 302 proxy in front of Lux intercepts,
 * `lux` is Lux's signed entry and `origin` is the `.strm` target itself.
 */
export type DirectCandidateKind = "proxy" | "lux" | "origin";

export type DirectCandidate = { kind: DirectCandidateKind; url: string };

/** Time a candidate gets to deliver metadata before the next one is tried. */
export const DIRECT_CANDIDATE_TIMEOUT_MS = 8_000;

const MEMORY_KEY = "lux.player.directCandidate";
const KINDS: readonly DirectCandidateKind[] = ["proxy", "lux", "origin"];

export function directCandidates(
  plan: WebPlaybackPlan | undefined,
  source: MediaSource | undefined,
): DirectCandidate[] {
  if (plan?.type !== "DIRECT") return [];
  const served = (plan.candidates ?? []).filter(
    (entry): entry is DirectCandidate =>
      KINDS.some((kind) => kind === entry.kind) && typeof entry.url === "string" && entry.url !== "",
  );
  // The server orders the ladder (and decides which entries exist); the legacy fields below are
  // only read when it did not send one.
  if (served.length > 0) return served;
  const origin = isRemoteHttpStrmSource(source) ? source?.externalUrl ?? null : null;
  const candidates: DirectCandidate[] = [];
  if (plan.proxyUrl) candidates.push({ kind: "proxy", url: plan.proxyUrl });
  // Without a proxy entry a remote STRM keeps playing from its own URL only.
  if (plan.url && (!origin || plan.proxyUrl)) candidates.push({ kind: "lux", url: plan.url });
  if (origin) candidates.push({ kind: "origin", url: origin });
  return candidates;
}

/** Moves the remembered kind to the front while keeping the others in order. */
export function orderCandidates(
  candidates: readonly DirectCandidate[],
  remembered: DirectCandidateKind | null,
): DirectCandidate[] {
  const first = candidates.find((candidate) => candidate.kind === remembered);
  return first ? [first, ...candidates.filter((candidate) => candidate !== first)] : [...candidates];
}

export function rememberedCandidate(): DirectCandidateKind | null {
  try {
    const value = window.localStorage.getItem(MEMORY_KEY);
    return KINDS.find((kind) => kind === value) ?? null;
  } catch {
    return null;
  }
}

export function rememberCandidate(kind: DirectCandidateKind) {
  try {
    window.localStorage.setItem(MEMORY_KEY, kind);
  } catch {
    // Storage may be unavailable (private mode); the ladder still works.
  }
}

export function clearRememberedCandidate() {
  try {
    window.localStorage.removeItem(MEMORY_KEY);
  } catch {
    // See rememberCandidate.
  }
}
