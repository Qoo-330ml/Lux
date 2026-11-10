import { vi } from "vitest";

/**
 * Shared stand-in for the browser `WebSocket` used by the realtime client tests.
 *
 * The production client only uses the four constructor constants, `readyState`, `url`, `send`,
 * `close` and `addEventListener`/`removeEventListener`, so this fake implements exactly that
 * surface. Nothing is dispatched automatically: a test decides when the handshake succeeds
 * (`open()`), when a frame arrives (`message()`), and when the connection ends
 * (`serverClose(code)`), which keeps the reconnect schedule observable under fake timers.
 */

type Listener = (event: Event) => void;

/** `CloseEvent` is not always present in jsdom; only `code` is read by the client. */
class FakeCloseEvent extends Event {
  readonly code: number;
  readonly reason: string;
  readonly wasClean: boolean;

  constructor(code: number, reason: string, wasClean: boolean) {
    super("close");
    this.code = code;
    this.reason = reason;
    this.wasClean = wasClean;
  }
}

export class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  /** Every socket the client has opened, oldest first. */
  static instances: FakeWebSocket[] = [];

  static reset() {
    FakeWebSocket.instances = [];
  }

  /** The most recent socket, or `undefined` when no connection was attempted yet. */
  static last(): FakeWebSocket | undefined {
    return FakeWebSocket.instances[FakeWebSocket.instances.length - 1];
  }

  readonly url: string;
  readonly protocols?: string | string[];
  readonly sent: string[] = [];
  readonly closeCalls: { code?: number; reason?: string }[] = [];
  readyState: number = FakeWebSocket.CONNECTING;
  private listeners = new Map<string, Set<Listener>>();

  constructor(url: string, protocols?: string | string[]) {
    this.url = url;
    this.protocols = protocols;
    FakeWebSocket.instances.push(this);
  }

  addEventListener(type: string, listener: Listener) {
    const listeners = this.listeners.get(type) ?? new Set<Listener>();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: string, listener: Listener) {
    this.listeners.get(type)?.delete(listener);
  }

  send(data: string) {
    if (this.readyState !== FakeWebSocket.OPEN) throw new Error("WebSocket is not open");
    this.sent.push(data);
  }

  close(code?: number, reason?: string) {
    this.closeCalls.push({ code, reason });
    this.readyState = FakeWebSocket.CLOSED;
  }

  /** Simulates a completed handshake. */
  open() {
    this.readyState = FakeWebSocket.OPEN;
    this.dispatch("open", new Event("open"));
  }

  /** Delivers a server frame; objects are serialized the way the server does. */
  message(payload: unknown) {
    const data = typeof payload === "string" ? payload : JSON.stringify(payload);
    this.dispatch("message", new MessageEvent("message", { data }));
  }

  /** Delivers the `ready` frame that marks a connection as established. */
  ready(version = 1) {
    this.open();
    this.message({ type: "ready", version });
  }

  /** Ends the connection from the server (or the network) side. */
  serverClose(code = 1006, reason = "") {
    this.readyState = FakeWebSocket.CLOSED;
    this.dispatch("close", new FakeCloseEvent(code, reason, code !== 1006));
  }

  /** Delivers a transport error; browsers follow this with `serverClose(1006)`. */
  serverError() {
    this.dispatch("error", new Event("error"));
  }

  private dispatch(type: string, event: Event) {
    for (const listener of [...(this.listeners.get(type) ?? [])]) listener(event);
  }
}

/** Installs the fake as the global `WebSocket` and clears previously created sockets. */
export function installFakeWebSocket(): typeof FakeWebSocket {
  FakeWebSocket.reset();
  vi.stubGlobal("WebSocket", FakeWebSocket);
  return FakeWebSocket;
}

/**
 * Overrides `document.hidden`, which jsdom otherwise always reports as `false`.
 * Returns a function that restores the original definition.
 */
export function setDocumentHidden(hidden: boolean): () => void {
  const descriptor = Object.getOwnPropertyDescriptor(document, "hidden");
  Object.defineProperty(document, "hidden", { configurable: true, value: hidden });
  return () => {
    if (descriptor) {
      Object.defineProperty(document, "hidden", descriptor);
      return;
    }
    delete (document as Document & { hidden?: boolean }).hidden;
  };
}

/** Fires the browser `offline` / `online` notifications the realtime client listens for. */
export function dispatchNetworkStatus(online: boolean) {
  window.dispatchEvent(new Event(online ? "online" : "offline"));
}

/** Fires the `visibilitychange` notification after `setDocumentHidden` changed the value. */
export function dispatchVisibilityChange() {
  document.dispatchEvent(new Event("visibilitychange"));
}
