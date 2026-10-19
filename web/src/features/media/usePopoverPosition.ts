import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

export type PopoverPosition = { left: number; top: number };
export type PopoverSize = { width: number; height: number };
export type PopoverViewport = { width: number; height: number };

type UsePopoverPositionOptions = {
  open: boolean;
  onClose: () => void;
  isInside?: (target: Node) => boolean;
  closeOnResize?: boolean;
  closeOnContextMenu?: boolean;
  fallbackMenuSize: PopoverSize;
  getPosition: (menu: PopoverSize, viewport: PopoverViewport) => PopoverPosition;
};

export function usePopoverPosition(options: UsePopoverPositionOptions) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<PopoverPosition>({ left: 0, top: 0 });
  const optionsRef = useRef(options);
  optionsRef.current = options;

  const updatePosition = useCallback(() => {
    const menu = menuRef.current;
    if (!menu) return;

    const bounds = menu.getBoundingClientRect();
    const current = optionsRef.current;
    setPosition(current.getPosition(
      {
        width: bounds.width || current.fallbackMenuSize.width,
        height: bounds.height || current.fallbackMenuSize.height,
      },
      { width: window.innerWidth, height: window.innerHeight },
    ));
  }, []);

  useLayoutEffect(() => {
    if (!options.open) return undefined;
    updatePosition();

    const handleResize = () => {
      if (optionsRef.current.closeOnResize) {
        optionsRef.current.onClose();
      } else {
        updatePosition();
      }
    };

    window.addEventListener("resize", handleResize);
    window.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", handleResize);
      window.removeEventListener("scroll", updatePosition, true);
    };
  }, [options.open, updatePosition]);

  useEffect(() => {
    if (!options.open) return undefined;

    const isInside = (target: EventTarget | null): boolean => {
      if (!(target instanceof Node)) return false;
      return Boolean(menuRef.current?.contains(target) || optionsRef.current.isInside?.(target));
    };
    const closeOnOutsidePointer = (event: PointerEvent) => {
      if (!isInside(event.target)) optionsRef.current.onClose();
    };
    const closeOnOutsideContextMenu = (event: MouseEvent) => {
      if (optionsRef.current.closeOnContextMenu && !isInside(event.target)) {
        optionsRef.current.onClose();
      }
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") optionsRef.current.onClose();
    };
    const closeOnContextMenu = optionsRef.current.closeOnContextMenu;

    document.addEventListener("pointerdown", closeOnOutsidePointer);
    if (closeOnContextMenu) document.addEventListener("contextmenu", closeOnOutsideContextMenu);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointer);
      if (closeOnContextMenu) document.removeEventListener("contextmenu", closeOnOutsideContextMenu);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [options.open]);

  return { menuRef, position };
}
