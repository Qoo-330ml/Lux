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

function scrollStateFromMetrics(rail: HTMLDivElement): ScrollState {
  const maxScrollLeft = Math.max(0, rail.scrollWidth - rail.clientWidth);
  return {
    canScrollLeft: rail.scrollLeft > 1,
    canScrollRight: maxScrollLeft - rail.scrollLeft > 1,
  };
}

function edgeItemsFor(rail: HTMLDivElement) {
  const firstContent = rail.firstElementChild;
  const lastContent = rail.lastElementChild;
  return {
    first: firstContent?.firstElementChild ?? firstContent,
    last: lastContent?.lastElementChild ?? lastContent,
  };
}

function maxScrollLeftFor(rail: HTMLDivElement) {
  const measuredMax = Math.max(0, rail.scrollWidth - rail.clientWidth);
  const { last } = edgeItemsFor(rail);
  const railRect = rail.getBoundingClientRect();
  if (!last || railRect.width <= 1) return measuredMax;

  const lastRect = last.getBoundingClientRect();
  if (lastRect.width <= 0) return measuredMax;
  return Math.max(measuredMax, rail.scrollLeft + Math.max(0, lastRect.right - railRect.right));
}

function sameScrollState(left: ScrollState, right: ScrollState) {
  return left.canScrollLeft === right.canScrollLeft && left.canScrollRight === right.canScrollRight;
}

export function HorizontalScrollRail({ ariaLabel, children, className }: HorizontalScrollRailProps) {
  const railRef = useRef<HTMLDivElement>(null);
  const [scrollState, setScrollState] = useState<ScrollState>({ canScrollLeft: false, canScrollRight: false });

  const setScrollStateIfChanged = useCallback((nextState: ScrollState) => {
    setScrollState((current) => (sameScrollState(current, nextState) ? current : nextState));
  }, []);

  useEffect(() => {
    const rail = railRef.current;
    if (!rail) return undefined;

    let animationFrame = 0;
    let firstTarget: Element | null = null;
    let lastTarget: Element | null = null;
    let intersectionObserver: IntersectionObserver | undefined;
    let resizeObserver: ResizeObserver | undefined;
    let mutationObserver: MutationObserver | undefined;

    const requestFrame = window.requestAnimationFrame?.bind(window)
      ?? ((callback: FrameRequestCallback) => window.setTimeout(() => callback(performance.now()), 16));
    const cancelFrame = window.cancelAnimationFrame?.bind(window) ?? window.clearTimeout.bind(window);

    const scheduleMetricUpdate = () => {
      if (animationFrame) return;
      animationFrame = requestFrame(() => {
        animationFrame = 0;
        setScrollStateIfChanged(scrollStateFromMetrics(rail));
      });
    };

    const observeEdges = (refresh = false) => {
      const previousFirst = firstTarget;
      const previousLast = lastTarget;
      const next = edgeItemsFor(rail);
      if (intersectionObserver) {
        if (previousFirst && (refresh || previousFirst !== next.first)) intersectionObserver.unobserve(previousFirst);
        if (previousLast && previousLast !== previousFirst && (refresh || previousLast !== next.last)) {
          intersectionObserver.unobserve(previousLast);
        }

        firstTarget = next.first;
        lastTarget = next.last;
        if (firstTarget) intersectionObserver.observe(firstTarget);
        if (lastTarget && lastTarget !== firstTarget) intersectionObserver.observe(lastTarget);
      }

      if (resizeObserver) {
        if (previousFirst && (refresh || previousFirst !== next.first)) resizeObserver.unobserve(previousFirst);
        if (previousLast && previousLast !== previousFirst && (refresh || previousLast !== next.last)) {
          resizeObserver.unobserve(previousLast);
        }
        if (firstTarget) resizeObserver.observe(firstTarget);
        if (lastTarget && lastTarget !== firstTarget) resizeObserver.observe(lastTarget);
      }

      if (!firstTarget && !lastTarget) scheduleMetricUpdate();
    };

    if (typeof IntersectionObserver !== "undefined") {
      intersectionObserver = new IntersectionObserver((entries) => {
        setScrollState((current) => {
          const next = { ...current };
          for (const entry of entries) {
            if (entry.target !== firstTarget && entry.target !== lastTarget) continue;

            if (!entry.rootBounds) {
              Object.assign(next, scrollStateFromMetrics(rail));
              continue;
            }

            if (entry.target === firstTarget) {
              next.canScrollLeft = entry.boundingClientRect.left < entry.rootBounds.left - 1;
            }
            if (entry.target === lastTarget) {
              next.canScrollRight = entry.boundingClientRect.right > entry.rootBounds.right + 1;
            }
          }
          return sameScrollState(current, next) ? current : next;
        });
      }, { root: rail, threshold: [0, 1] });
    } else {
      rail.addEventListener("scroll", scheduleMetricUpdate, { passive: true });
    }

    if (typeof ResizeObserver !== "undefined") {
      resizeObserver = new ResizeObserver(() => {
        if (intersectionObserver) observeEdges(true);
        else scheduleMetricUpdate();
      });
      resizeObserver.observe(rail);
      for (const child of rail.children) resizeObserver.observe(child);
    }

    if (typeof MutationObserver !== "undefined") {
      mutationObserver = new MutationObserver(() => {
        if (intersectionObserver) observeEdges(true);
        else scheduleMetricUpdate();
      });
      mutationObserver.observe(rail, { childList: true, subtree: true });
    }

    observeEdges();
    if (!intersectionObserver) scheduleMetricUpdate();

    return () => {
      if (animationFrame) cancelFrame(animationFrame);
      rail.removeEventListener("scroll", scheduleMetricUpdate);
      intersectionObserver?.disconnect();
      resizeObserver?.disconnect();
      mutationObserver?.disconnect();
    };
  }, [setScrollStateIfChanged]);

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
      setScrollStateIfChanged({
        canScrollLeft: targetScrollLeft > 1,
        canScrollRight: maxScrollLeft - targetScrollLeft > 1,
      });
    } else {
      rail.scrollTo({ left: targetScrollLeft, behavior: "smooth" });
    }
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
      <div ref={railRef} className="lux-horizontal-scroll-viewport">
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
