import { useLayoutEffect, useState, type RefObject } from "react";
import { heroOrientation, type HeroOrientation } from "./hero-preload";

/**
 * Orientation of the element's own box, re-evaluated whenever it is resized. Reading the box
 * (not the device) makes a phone, a rotated tablet, a split view and a narrow desktop window all
 * pick artwork the same way. It stays `undefined` until the first layout measurement, which runs
 * before paint, so callers can hold back the image request instead of fetching a wrong guess.
 */
export function useHeroOrientation(ref: RefObject<HTMLElement | null>): HeroOrientation | undefined {
  const [orientation, setOrientation] = useState<HeroOrientation>();

  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return undefined;
    const measure = () => {
      const { width, height } = element.getBoundingClientRect();
      setOrientation((previous) => heroOrientation(width, height, previous));
    };
    measure();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", measure);
      return () => window.removeEventListener("resize", measure);
    }
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref]);

  return orientation;
}
