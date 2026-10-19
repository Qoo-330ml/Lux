// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from "vitest";
import { attachTouchCardHoverTracking } from "../src/app/touch-card-hover";

function createFrameScheduler() {
  let nextFrame: FrameRequestCallback | undefined;
  const requestAnimationFrame = vi.fn((callback: FrameRequestCallback) => {
    nextFrame = callback;
    return 17;
  });
  const cancelAnimationFrame = vi.fn(() => {
    nextFrame = undefined;
  });

  return {
    requestAnimationFrame,
    cancelAnimationFrame,
    flush(timestamp = 0) {
      const callback = nextFrame;
      nextFrame = undefined;
      callback?.(timestamp);
    },
  };
}

function pointerEvent(
  type: "pointermove" | "pointerdown" | "pointerup",
  { pointerType = "touch", isPrimary = true, x = 0, y = 0 }: {
    pointerType?: string;
    isPrimary?: boolean;
    x?: number;
    y?: number;
  } = {},
) {
  const event = new Event(type, { bubbles: true });
  Object.defineProperties(event, {
    pointerType: { value: pointerType },
    isPrimary: { value: isPrimary },
    clientX: { value: x },
    clientY: { value: y },
  });
  return event;
}

describe("touch card hover tracking", () => {
  let dispose: (() => void) | undefined;
  let firstCard: HTMLDivElement;
  let secondCard: HTMLDivElement;
  let restoreElementFromPoint: (() => void) | undefined;

  afterEach(() => {
    dispose?.();
    dispose = undefined;
    firstCard?.remove();
    secondCard?.remove();
    restoreElementFromPoint?.();
    restoreElementFromPoint = undefined;
  });

  it("checks the last touch position once per frame and keeps its card active after lift", () => {
    firstCard = document.createElement("div");
    firstCard.className = "lux-media-art-shell";
    secondCard = document.createElement("div");
    secondCard.className = "lux-library-card-cover-shell";
    document.body.append(firstCard, secondCard);
    const elementFromPoint = vi.fn((x: number) => x < 20 ? firstCard : secondCard);
    const previousDescriptor = Object.getOwnPropertyDescriptor(document, "elementFromPoint");
    Object.defineProperty(document, "elementFromPoint", { configurable: true, value: elementFromPoint });
    restoreElementFromPoint = () => {
      if (previousDescriptor) Object.defineProperty(document, "elementFromPoint", previousDescriptor);
      else Reflect.deleteProperty(document, "elementFromPoint");
    };
    const scheduler = createFrameScheduler();
    dispose = attachTouchCardHoverTracking(document, scheduler);

    document.dispatchEvent(pointerEvent("pointermove", { x: 1, y: 5 }));
    document.dispatchEvent(pointerEvent("pointermove", { x: 10, y: 5 }));
    document.dispatchEvent(pointerEvent("pointermove", { x: 30, y: 5 }));

    expect(scheduler.requestAnimationFrame).toHaveBeenCalledOnce();
    expect(elementFromPoint).not.toHaveBeenCalled();

    scheduler.flush(16);

    expect(elementFromPoint).toHaveBeenCalledOnce();
    expect(elementFromPoint).toHaveBeenCalledWith(30, 5);
    expect(firstCard.classList.contains("is-touch-hovered")).toBe(false);
    expect(secondCard.classList.contains("is-touch-hovered")).toBe(true);
    document.dispatchEvent(pointerEvent("pointerup", { x: 30, y: 5 }));
    expect(secondCard.classList.contains("is-touch-hovered")).toBe(true);

    elementFromPoint.mockReturnValue(null);
    document.dispatchEvent(pointerEvent("pointerdown", { x: 400, y: 400 }));
    expect(secondCard.classList.contains("is-touch-hovered")).toBe(false);
  });

  it("ignores mouse and secondary-pointer movement", () => {
    firstCard = document.createElement("div");
    secondCard = document.createElement("div");
    const scheduler = createFrameScheduler();
    dispose = attachTouchCardHoverTracking(document, scheduler);

    document.dispatchEvent(pointerEvent("pointermove", { pointerType: "mouse" }));
    document.dispatchEvent(pointerEvent("pointermove", { isPrimary: false }));

    expect(scheduler.requestAnimationFrame).not.toHaveBeenCalled();
  });

  it("cancels pending work and removes active state when tracking is disposed", () => {
    firstCard = document.createElement("div");
    firstCard.className = "lux-media-art-shell";
    document.body.append(firstCard);
    const elementFromPoint = vi.fn(() => firstCard);
    const previousDescriptor = Object.getOwnPropertyDescriptor(document, "elementFromPoint");
    Object.defineProperty(document, "elementFromPoint", { configurable: true, value: elementFromPoint });
    restoreElementFromPoint = () => {
      if (previousDescriptor) Object.defineProperty(document, "elementFromPoint", previousDescriptor);
      else Reflect.deleteProperty(document, "elementFromPoint");
    };
    const scheduler = createFrameScheduler();
    dispose = attachTouchCardHoverTracking(document, scheduler);
    document.dispatchEvent(pointerEvent("pointermove", { x: 1 }));
    scheduler.flush();
    expect(firstCard.classList.contains("is-touch-hovered")).toBe(true);

    document.dispatchEvent(pointerEvent("pointermove", { x: 1 }));
    dispose();
    dispose = undefined;

    expect(scheduler.cancelAnimationFrame).toHaveBeenCalledWith(17);
    expect(firstCard.classList.contains("is-touch-hovered")).toBe(false);
    document.dispatchEvent(pointerEvent("pointermove", { x: 2 }));
    expect(scheduler.requestAnimationFrame).toHaveBeenCalledTimes(2);
  });
});
