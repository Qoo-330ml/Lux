// @vitest-environment jsdom

import { act, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { usePopoverPosition } from "../src/features/media/usePopoverPosition";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function PopoverHarness({
  onClose,
  closeOnResize = false,
  closeOnContextMenu = false,
}: {
  onClose: () => void;
  closeOnResize?: boolean;
  closeOnContextMenu?: boolean;
}) {
  const [open, setOpen] = useState(true);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const { menuRef, position } = usePopoverPosition({
    open,
    onClose: () => {
      onClose();
      setOpen(false);
    },
    isInside: (target) => Boolean(triggerRef.current?.contains(target)),
    closeOnResize,
    closeOnContextMenu,
    fallbackMenuSize: { width: 80, height: 40 },
    getPosition: (_menuSize, viewport) => ({ left: viewport.width - 80, top: viewport.height - 40 }),
  });

  return (
    <>
      <button ref={triggerRef} data-popover-trigger type="button">Open</button>
      {open ? createPortal(
        <div ref={menuRef} role="menu" style={{ left: position.left, top: position.top }} />,
        document.body,
      ) : null}
    </>
  );
}

describe("usePopoverPosition", () => {
  let container: HTMLDivElement;
  let root: Root;
  let restoreInnerWidth: (() => void) | undefined;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    restoreInnerWidth?.();
    restoreInnerWidth = undefined;
  });

  function renderPopover(options: {
    onClose?: () => void;
    closeOnResize?: boolean;
    closeOnContextMenu?: boolean;
  } = {}) {
    const onClose = options.onClose ?? vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    act(() => {
      root.render(
        <PopoverHarness
          onClose={onClose}
          closeOnResize={options.closeOnResize}
          closeOnContextMenu={options.closeOnContextMenu}
        />,
      );
    });
    return { onClose };
  }

  it("keeps pointer events inside the trigger and menu, then closes outside", () => {
    const { onClose } = renderPopover();
    const trigger = container.querySelector<HTMLButtonElement>("[data-popover-trigger]");
    const menu = document.body.querySelector<HTMLElement>("[role=menu]");
    const outside = document.createElement("button");
    document.body.append(outside);

    act(() => trigger?.dispatchEvent(new Event("pointerdown", { bubbles: true })));
    expect(document.body.querySelector("[role=menu]")).not.toBeNull();
    act(() => menu?.dispatchEvent(new Event("pointerdown", { bubbles: true })));
    expect(document.body.querySelector("[role=menu]")).not.toBeNull();
    act(() => outside.dispatchEvent(new Event("pointerdown", { bubbles: true })));

    expect(document.body.querySelector("[role=menu]")).toBeNull();
    expect(onClose).toHaveBeenCalledOnce();
    outside.remove();
  });

  it("closes on Escape", () => {
    const { onClose } = renderPopover();

    act(() => document.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" })));
    expect(document.body.querySelector("[role=menu]")).not.toBeNull();
    act(() => document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })));

    expect(document.body.querySelector("[role=menu]")).toBeNull();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("closes on outside context menus when enabled", () => {
    const { onClose } = renderPopover({ closeOnContextMenu: true });
    const outside = document.createElement("button");
    document.body.append(outside);

    act(() => outside.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true })));

    expect(document.body.querySelector("[role=menu]")).toBeNull();
    expect(onClose).toHaveBeenCalledOnce();
    outside.remove();
  });

  it("repositions on scroll and resize when resize closing is disabled", () => {
    renderPopover();
    const menu = document.body.querySelector<HTMLElement>("[role=menu]");
    expect(menu?.style.left).toBe(`${window.innerWidth - 80}px`);
    const originalInnerWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 320 });
    restoreInnerWidth = () => {
      if (originalInnerWidth) Object.defineProperty(window, "innerWidth", originalInnerWidth);
      else Reflect.deleteProperty(window, "innerWidth");
    };

    act(() => window.dispatchEvent(new Event("scroll")));

    expect(menu?.style.left).toBe("240px");
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 240 });
    act(() => window.dispatchEvent(new Event("resize")));
    expect(menu?.style.left).toBe("160px");
  });

  it("closes on resize when requested", () => {
    const { onClose } = renderPopover({ closeOnResize: true });

    act(() => window.dispatchEvent(new Event("resize")));

    expect(document.body.querySelector("[role=menu]")).toBeNull();
    expect(onClose).toHaveBeenCalledOnce();
  });
});
