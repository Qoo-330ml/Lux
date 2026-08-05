import { ChevronLeft, ChevronRight } from "lucide-react";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";

type HorizontalScrollRailProps = {
  ariaLabel: string;
  children: ReactNode;
  className?: string;
};

type ScrollState = {
  canScrollLeft: boolean;
  canScrollRight: boolean;
};

function scrollStateFor(rail: HTMLDivElement): ScrollState {
  const maxScrollLeft = maxScrollLeftFor(rail);
  const content = rail.firstElementChild;
  const firstItem = content?.firstElementChild;
  const lastItem = content?.lastElementChild;
  const railRect = rail.getBoundingClientRect();

  if (firstItem && lastItem && railRect.width > 1) {
    const firstRect = firstItem.getBoundingClientRect();
    const lastRect = lastItem.getBoundingClientRect();
    if (firstRect.width > 0 && lastRect.width > 0) {
      return {
        canScrollLeft: firstRect.left < railRect.left - 1,
        canScrollRight: lastRect.right > railRect.right + 1,
      };
    }
  }

  return {
    canScrollLeft: rail.scrollLeft > 1,
    canScrollRight: maxScrollLeft - rail.scrollLeft > 1,
  };
}

function maxScrollLeftFor(rail: HTMLDivElement) {
  const measuredMax = Math.max(0, rail.scrollWidth - rail.clientWidth);
  const content = rail.firstElementChild;
  const lastItem = content?.lastElementChild;
  const railRect = rail.getBoundingClientRect();
  if (!lastItem || railRect.width <= 1) return measuredMax;

  const lastRect = lastItem.getBoundingClientRect();
  if (lastRect.width <= 0) return measuredMax;
  return Math.max(measuredMax, rail.scrollLeft + Math.max(0, lastRect.right - railRect.right));
}

export function HorizontalScrollRail({ ariaLabel, children, className }: HorizontalScrollRailProps) {
  const railRef = useRef<HTMLDivElement>(null);
  const [scrollState, setScrollState] = useState<ScrollState>({ canScrollLeft: false, canScrollRight: false });

  const measureScroll = useCallback(() => {
    const rail = railRef.current;
    if (!rail) return;

    const nextState = scrollStateFor(rail);
    setScrollState((current) => (
      current.canScrollLeft === nextState.canScrollLeft && current.canScrollRight === nextState.canScrollRight
        ? current
        : nextState
    ));
  }, []);

  useEffect(() => {
    const rail = railRef.current;
    if (!rail) return undefined;

    measureScroll();
    rail.addEventListener("scroll", measureScroll, { passive: true });

    if (typeof ResizeObserver === "undefined") {
      return () => rail.removeEventListener("scroll", measureScroll);
    }

    const observer = new ResizeObserver(measureScroll);
    observer.observe(rail);
    if (rail.firstElementChild) observer.observe(rail.firstElementChild);

    return () => {
      rail.removeEventListener("scroll", measureScroll);
      observer.disconnect();
    };
  }, [children, measureScroll]);

  function scrollByPage(direction: -1 | 1) {
    const rail = railRef.current;
    if (!rail) return;

    const maxScrollLeft = maxScrollLeftFor(rail);
    const targetScrollLeft = Math.min(
      maxScrollLeft,
      Math.max(0, rail.scrollLeft + direction * rail.clientWidth * 0.8),
    );
    if (targetScrollLeft <= 1 || maxScrollLeft - targetScrollLeft <= 1) {
      rail.scrollLeft = targetScrollLeft;
    } else {
      rail.scrollTo({ left: targetScrollLeft, behavior: "smooth" });
    }
    setScrollState({
      canScrollLeft: targetScrollLeft > 1,
      canScrollRight: maxScrollLeft - targetScrollLeft > 1,
    });
  }

  return (
    <div className={`lux-horizontal-scroll-rail${className ? ` ${className}` : ""}`}>
      {scrollState.canScrollLeft ? (
        <button
          className="lux-horizontal-scroll-arrow is-left"
          type="button"
          aria-label={`向左滚动${ariaLabel}`}
          onClick={() => scrollByPage(-1)}
        >
          <ChevronLeft size={18} aria-hidden="true" />
        </button>
      ) : null}
      <div ref={railRef} className="lux-horizontal-scroll-viewport" onScroll={measureScroll}>
        {children}
      </div>
      {scrollState.canScrollRight ? (
        <button
          className="lux-horizontal-scroll-arrow is-right"
          type="button"
          aria-label={`向右滚动${ariaLabel}`}
          onClick={() => scrollByPage(1)}
        >
          <ChevronRight size={18} aria-hidden="true" />
        </button>
      ) : null}
    </div>
  );
}
