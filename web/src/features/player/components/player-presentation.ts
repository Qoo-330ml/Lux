import type { CSSProperties } from "react";

export type PlayerAspectRatio = "default" | "4:3" | "16:9";
export type PlayerFlip = "normal" | "horizontal" | "vertical";

export type PlayerVideoPresentation = {
  loop: boolean;
  aspectRatio: PlayerAspectRatio;
  flip: PlayerFlip;
};

export const DEFAULT_VIDEO_PRESENTATION: PlayerVideoPresentation = {
  loop: false,
  aspectRatio: "default",
  flip: "normal",
};

export type PlayerVideoBoxSize = {
  width: number;
  height: number;
};

export function playerVideoPresentationSize(
  aspectRatio: PlayerAspectRatio,
  containerWidth: number,
  containerHeight: number,
): PlayerVideoBoxSize | null {
  if (
    aspectRatio === "default"
    || !Number.isFinite(containerWidth)
    || !Number.isFinite(containerHeight)
    || containerWidth <= 0
    || containerHeight <= 0
  ) return null;

  const ratio = aspectRatio === "4:3" ? 4 / 3 : 16 / 9;
  if (containerWidth / containerHeight > ratio) {
    return { width: ratio * containerHeight, height: containerHeight };
  }
  return { width: containerWidth, height: containerWidth / ratio };
}

export function playerVideoPresentationStyle(
  aspectRatio: PlayerAspectRatio,
  flip: PlayerFlip,
  size: PlayerVideoBoxSize | null = null,
): CSSProperties | undefined {
  const flipTransform = flip === "horizontal"
    ? "scaleX(-1)"
    : flip === "vertical"
      ? "scaleY(-1)"
      : "";
  if (aspectRatio === "default") {
    return flipTransform ? { transform: flipTransform } : undefined;
  }

  return {
    inset: "auto",
    top: "50%",
    left: "50%",
    width: size ? `${size.width}px` : "auto",
    height: size ? `${size.height}px` : "100%",
    maxWidth: "100%",
    maxHeight: "100%",
    aspectRatio: aspectRatio === "4:3" ? "4 / 3" : "16 / 9",
    objectFit: "fill",
    transform: `translate(-50%, -50%)${flipTransform ? ` ${flipTransform}` : ""}`,
  };
}
