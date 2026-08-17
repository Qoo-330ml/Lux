export function isHevcCodec(codec: string | null | undefined) {
  const normalized = codec?.trim().toLowerCase() ?? "";
  return normalized === "hevc" || normalized === "h265" || normalized.startsWith("hvc1.") || normalized.startsWith("hev1.");
}
