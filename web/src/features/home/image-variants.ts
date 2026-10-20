import { createContext, useContext } from "react";

export type ImageVariantWidth = 360 | 720 | 1280;

export type ImageVariantsState = {
  /** Server-level switch (管理员主开关) is on. */
  available: boolean;
  /** Server switch and the current user's preference are both on. */
  enabled: boolean;
};

export const IMAGE_VARIANTS_DISABLED: ImageVariantsState = {
  available: false,
  enabled: false,
};

export const ImageVariantsContext = createContext<ImageVariantsState>(IMAGE_VARIANTS_DISABLED);

export function useImageVariants(): ImageVariantsState {
  return useContext(ImageVariantsContext);
}

export function imageVariantWidth(enabled: boolean, width: ImageVariantWidth): number | undefined {
  return enabled ? width : undefined;
}

/** Append `w=` to an image URL that may already carry a `tag=` query. */
export function withImageWidth(url: string, width?: number): string {
  if (!width) return url;
  return `${url}${url.includes("?") ? "&" : "?"}w=${width}`;
}
