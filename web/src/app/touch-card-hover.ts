export type TouchHoverFrameScheduler = Pick<Window, "requestAnimationFrame" | "cancelAnimationFrame">;

export function attachTouchCardHoverTracking(
  documentRef: Document,
  scheduler: TouchHoverFrameScheduler,
): () => void {
  let activeCard: HTMLElement | null = null;
  let pendingPointer: { x: number; y: number } | null = null;
  let pendingFrame: number | undefined;

  const cardAt = (x: number, y: number) => documentRef.elementFromPoint(x, y)
    ?.closest<HTMLElement>(".lux-media-art-shell, .lux-library-card-cover-shell") ?? null;

  const setActiveCard = (nextCard: HTMLElement | null) => {
    if (activeCard === nextCard) return;
    activeCard?.classList.remove("is-touch-hovered");
    activeCard = nextCard;
    activeCard?.classList.add("is-touch-hovered");
  };

  const cancelPendingFrame = () => {
    if (pendingFrame === undefined) return;
    scheduler.cancelAnimationFrame(pendingFrame);
    pendingFrame = undefined;
    pendingPointer = null;
  };

  const revealCardAtPointer = (event: PointerEvent) => {
    if (event.pointerType !== "touch" || !event.isPrimary) return;
    pendingPointer = { x: event.clientX, y: event.clientY };
    if (pendingFrame !== undefined) return;

    pendingFrame = scheduler.requestAnimationFrame(() => {
      pendingFrame = undefined;
      const pointer = pendingPointer;
      pendingPointer = null;
      if (pointer) setActiveCard(cardAt(pointer.x, pointer.y));
    });
  };

  const clearCardWhenTouchingElsewhere = (event: PointerEvent) => {
    if (event.pointerType !== "touch" || !event.isPrimary) return;
    cancelPendingFrame();
    if (cardAt(event.clientX, event.clientY) !== activeCard) setActiveCard(null);
  };

  documentRef.addEventListener("pointermove", revealCardAtPointer, { passive: true });
  documentRef.addEventListener("pointerdown", clearCardWhenTouchingElsewhere, { passive: true });

  return () => {
    documentRef.removeEventListener("pointermove", revealCardAtPointer);
    documentRef.removeEventListener("pointerdown", clearCardWhenTouchingElsewhere);
    cancelPendingFrame();
    setActiveCard(null);
  };
}
