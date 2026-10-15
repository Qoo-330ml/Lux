import { Component, Suspense, useEffect, useRef, type ReactNode } from "react";
import { X } from "lucide-react";

type EditorBoundaryProps = { children: ReactNode; onClose: () => void };

export function MediaEditorBoundary({ children, onClose }: EditorBoundaryProps) {
  return <EditorErrorBoundary onClose={onClose}>
    <Suspense fallback={<EditorLoadState onClose={onClose} />}>{children}</Suspense>
  </EditorErrorBoundary>;
}

class EditorErrorBoundary extends Component<EditorBoundaryProps, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  render() {
    return this.state.failed
      ? <EditorLoadState failed onClose={this.props.onClose} />
      : this.props.children;
  }
}

function EditorLoadState({ failed = false, onClose }: { failed?: boolean; onClose: () => void }) {
  const closeRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    closeRef.current?.focus();
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.body.style.overflow = previousOverflow;
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [onClose]);

  return <div className="lux-media-editor-backdrop" role="presentation" onMouseDown={(event) => {
    if (event.target === event.currentTarget) onClose();
  }}>
    <section className="lux-media-editor" role="dialog" aria-modal="true" aria-label="媒体编辑器" aria-busy={!failed}>
      <header className="lux-media-editor-header">
        <p role={failed ? "alert" : "status"}>{failed ? "编辑器加载失败，请重新加载后重试。" : "正在加载编辑器…"}</p>
        <button ref={closeRef} className="lux-media-editor-close" type="button" aria-label="关闭编辑器" onClick={onClose}><X size={18} /></button>
      </header>
      {failed ? <footer className="lux-media-editor-footer">
        <button className="lux-button lux-button-secondary" type="button" onClick={() => window.location.reload()}>重新加载</button>
      </footer> : null}
    </section>
  </div>;
}
