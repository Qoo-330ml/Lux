import { X } from "lucide-react";
import { useEffect, useRef, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from "react";
import { cx } from "./cx";

export type LuxDialogProps = {
  title: ReactNode;
  description?: ReactNode;
  children?: ReactNode;
  footer?: ReactNode;
  onClose?: () => void;
  /** 危险操作使用 alertdialog 语义。 */
  role?: "dialog" | "alertdialog";
  size?: "default" | "wide";
  /** 点击遮罩关闭，默认允许。 */
  closeOnBackdrop?: boolean;
  labelledBy?: string;
  className?: string;
};

/**
 * Lux 模态壳：玻璃面板、accnet 描边、ESC 关闭、焦点回收。
 * 保持 role="dialog"/"alertdialog" + aria-modal，替代各页面手写的 backdrop。
 */
export function LuxDialog({
  title,
  description,
  children,
  footer,
  onClose,
  role = "dialog",
  size = "default",
  closeOnBackdrop = true,
  labelledBy,
  className,
}: LuxDialogProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const titleId = "lux-dialog-title";
  const descriptionId = description === undefined ? undefined : "lux-dialog-description";

  useEffect(() => {
    const previous = typeof document === "undefined" ? null : document.activeElement;
    panelRef.current?.focus();
    const { overflow } = document.body.style;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = overflow;
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);

  const handleKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.stopPropagation();
      onClose?.();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = panelRef.current?.querySelectorAll<HTMLElement>(
      "button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])",
    );
    if (!focusable || focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first?.focus();
    }
  };

  return (
    <div
      className="lux-dialog-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (!closeOnBackdrop || event.target !== event.currentTarget) return;
        onClose?.();
      }}
    >
      <div
        ref={panelRef}
        className={cx("lux-dialog", size === "wide" && "lux-dialog-wide", className)}
        role={role}
        aria-modal="true"
        aria-labelledby={labelledBy ?? titleId}
        aria-describedby={descriptionId}
        tabIndex={-1}
        onKeyDown={handleKeyDown}
        data-lux-control="dialog"
      >
        <header className="lux-dialog-header">
          <div className="lux-dialog-heading">
            <h2 id={titleId} className="lux-dialog-title">{title}</h2>
            {description === undefined ? null : <p id={descriptionId} className="lux-dialog-description">{description}</p>}
          </div>
          {onClose ? (
            <button type="button" className="lux-icon-button lux-icon-button-small lux-dialog-close" aria-label="关闭" title="关闭" onClick={onClose}>
              <X size={16} aria-hidden="true" />
            </button>
          ) : null}
        </header>
        {children === undefined ? null : <div className="lux-dialog-body">{children}</div>}
        {footer === undefined ? null : <footer className="lux-dialog-footer">{footer}</footer>}
      </div>
    </div>
  );
}
