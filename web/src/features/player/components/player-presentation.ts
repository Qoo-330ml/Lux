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

export function playerVideoPresentationStyle(
  aspectRatio: PlayerAspectRatio,
  flip: PlayerFlip,
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
    width: "auto",
    height: "100%",
    maxWidth: "100%",
    maxHeight: "100%",
    aspectRatio: aspectRatio === "4:3" ? "4 / 3" : "16 / 9",
    objectFit: "fill",
    transform: `translate(-50%, -50%)${flipTransform ? ` ${flipTransform}` : ""}`,
  };
}
