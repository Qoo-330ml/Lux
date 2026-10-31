import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { LuxButton } from "./LuxButton";
import { LuxDialog } from "./LuxDialog";

export type LuxConfirmTone = "primary" | "danger";

export type LuxConfirmOptions = {
  title: string;
  description?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  tone?: LuxConfirmTone;
};

/** 字符串形式等价于 `{ title }`，便于从 window.confirm 直接迁移。 */
export type LuxConfirmInput = string | LuxConfirmOptions;

type LuxConfirmRequest = {
  title: string;
  description?: string;
  confirmLabel: string;
  cancelLabel: string;
  tone: LuxConfirmTone;
  resolve: (accepted: boolean) => void;
};

export type LuxConfirmFn = (input: LuxConfirmInput) => Promise<boolean>;

const LuxConfirmContext = createContext<LuxConfirmFn | null>(null);

function normalize(input: LuxConfirmInput): LuxConfirmOptions {
  return typeof input === "string" ? { title: input } : input;
}

/**
 * 全局确认弹窗宿主。应用根部挂载一次；未挂载时 useLuxConfirm 回退到
 * 浏览器确认框，保证独立渲染的页面测试仍然可预测。
 */
export function LuxConfirmProvider({ children }: { children: ReactNode }) {
  const [request, setRequest] = useState<LuxConfirmRequest | null>(null);
  const requestRef = useRef<LuxConfirmRequest | null>(null);

  const confirm = useCallback<LuxConfirmFn>((input) => new Promise<boolean>((resolve) => {
    const options = normalize(input);
    const next: LuxConfirmRequest = {
      title: options.title,
      description: options.description,
      confirmLabel: options.confirmLabel ?? "确定",
      cancelLabel: options.cancelLabel ?? "取消",
      tone: options.tone ?? "primary",
      resolve,
    };
    const pending = requestRef.current;
    requestRef.current = next;
    setRequest(next);
    if (pending) {
      pending.resolve(false);
    }
  }), []);

  const settle = useCallback((accepted: boolean) => {
    const current = requestRef.current;
    requestRef.current = null;
    setRequest(null);
    current?.resolve(accepted);
  }, []);

  const value = useMemo(() => confirm, [confirm]);

  return (
    <LuxConfirmContext.Provider value={value}>
      {children}
      {request ? (
        <LuxDialog
          role="alertdialog"
          title={request.title}
          description={request.description}
          closeOnBackdrop={false}
          onClose={() => settle(false)}
          className="lux-confirm"
          footer={
            <>
              <LuxButton tone="secondary" onClick={() => settle(false)}>{request.cancelLabel}</LuxButton>
              <LuxButton tone={request.tone === "danger" ? "danger" : "primary"} onClick={() => settle(true)} autoFocus>
                {request.confirmLabel}
              </LuxButton>
            </>
          }
        />
      ) : null}
    </LuxConfirmContext.Provider>
  );
}

/**
 * 返回 Lux 风格的异步确认函数。支持 `confirm("文案")` 与对象形式；
 * 没有 Provider 时回退到浏览器原生确认，消息保持与原调用一致。
 */
export function useLuxConfirm(): LuxConfirmFn {
  const provided = useContext(LuxConfirmContext);
  const fallback = useCallback<LuxConfirmFn>((input) => {
    const options = normalize(input);
    const message = options.description ? `${options.title}\n${options.description}` : options.title;
    const accepted = typeof window === "undefined" ? false : window.confirm(message);
    return Promise.resolve(accepted);
  }, []);
  return provided ?? fallback;
}
