// @vitest-environment jsdom

import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  PLAYER_SHORTCUTS,
  usePlayerKeyboardShortcuts,
  type PlayerShortcutAction,
} from "../src/features/player/usePlayerKeyboardShortcuts";
import { PlayerShortcutHelp } from "../src/features/player/components/player-shortcut-help";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement | undefined;
let root: Root | undefined;

afterEach(() => {
  if (root) act(() => root?.unmount());
  root = undefined;
  container?.remove();
  container = undefined;
  vi.restoreAllMocks();
});

function keydown(key: string, init: KeyboardEventInit = {}) {
  const event = new KeyboardEvent("keydown", { key, cancelable: true, bubbles: true, ...init });
  act(() => {
    window.dispatchEvent(event);
  });
  return event;
}

type ShortcutSpies = {
  togglePlayPause: ReturnType<typeof vi.fn>;
  seekRelative: ReturnType<typeof vi.fn>;
  changeVolume: ReturnType<typeof vi.fn>;
  toggleMute: ReturnType<typeof vi.fn>;
  toggleFullscreen: ReturnType<typeof vi.fn>;
  setShowInfo: ReturnType<typeof vi.fn>;
  setShowSettings: ReturnType<typeof vi.fn>;
  navigate: ReturnType<typeof vi.fn>;
};

function createSpies(): ShortcutSpies {
  return {
    togglePlayPause: vi.fn(),
    seekRelative: vi.fn(),
    changeVolume: vi.fn(),
    toggleMute: vi.fn(),
    toggleFullscreen: vi.fn(),
    setShowInfo: vi.fn(),
    setShowSettings: vi.fn(),
    navigate: vi.fn(),
  };
}

function resetSpies(spies: ShortcutSpies) {
  for (const spy of Object.values(spies)) spy.mockClear();
}

function mountShortcutHarness() {
  const spies = createSpies();

  function Harness() {
    usePlayerKeyboardShortcuts({
      itemId: "movie-1",
      volume: 0.5,
      changeVolume: spies.changeVolume,
      seekRelative: spies.seekRelative,
      togglePlayPause: spies.togglePlayPause,
      toggleMute: spies.toggleMute,
      toggleFullscreen: spies.toggleFullscreen,
      resetControlsTimeout: () => undefined,
      showInfo: false,
      showSettings: false,
      setShowInfo: spies.setShowInfo,
      setShowSettings: spies.setShowSettings,
      navigate: spies.navigate,
    });
    return null;
  }

  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  act(() => root?.render(<Harness />));
  return { spies };
}

describe("player shortcut data source", () => {
  it("documents every handled key exactly once", () => {
    const eventKeys = PLAYER_SHORTCUTS.flatMap((shortcut) => [...shortcut.eventKeys]);
    expect(new Set(eventKeys).size).toBe(eventKeys.length);
    expect(eventKeys).toEqual(
      expect.arrayContaining([" ", "Escape", "?", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"]),
    );
    for (const shortcut of PLAYER_SHORTCUTS) {
      expect(shortcut.keys.length).toBeGreaterThan(0);
      expect(shortcut.description.length).toBeGreaterThan(0);
      expect(shortcut.group.length).toBeGreaterThan(0);
    }
  });

  it("keeps the handler in sync with the documented keys and actions", () => {
    const { spies } = mountShortcutHarness();
    const probes: Partial<Record<PlayerShortcutAction, () => void>> = {
      togglePlayPause: () => {
        expect(spies.togglePlayPause).toHaveBeenCalledTimes(1);
      },
      seekBackward: () => {
        expect(spies.seekRelative).toHaveBeenCalledWith(-10);
      },
      seekForward: () => {
        expect(spies.seekRelative).toHaveBeenCalledWith(10);
      },
      volumeUp: () => {
        expect(spies.changeVolume).toHaveBeenCalledTimes(1);
        expect(spies.changeVolume.mock.calls[0][0]).toBeCloseTo(0.55, 5);
      },
      volumeDown: () => {
        expect(spies.changeVolume).toHaveBeenCalledTimes(1);
        expect(spies.changeVolume.mock.calls[0][0]).toBeCloseTo(0.45, 5);
      },
      toggleInfo: () => {
        expect(spies.setShowInfo).toHaveBeenCalledTimes(1);
      },
      toggleMute: () => {
        expect(spies.toggleMute).toHaveBeenCalledTimes(1);
      },
      toggleFullscreen: () => {
        expect(spies.toggleFullscreen).toHaveBeenCalledTimes(1);
      },
      closeOverlay: () => {
        expect(spies.navigate).toHaveBeenCalledWith("/items/movie-1");
      },
    };

    for (const shortcut of PLAYER_SHORTCUTS) {
      const probe = probes[shortcut.action];
      // "?" is covered by the help panel tests below, which assert the toggle itself.
      if (!probe) continue;
      for (const key of shortcut.eventKeys) {
        resetSpies(spies);
        const event = keydown(key);
        expect(event.defaultPrevented, `${key} should be handled`).toBe(true);
        probe();
      }
    }

    resetSpies(spies);
    const unknown = keydown("z");
    expect(unknown.defaultPrevented).toBe(false);
    expect(spies.togglePlayPause).not.toHaveBeenCalled();
  });

  it("lists the help toggle and the overlay close key in the same data source", () => {
    const actions = PLAYER_SHORTCUTS.map((shortcut) => shortcut.action);
    expect(actions).toContain("toggleShortcutHelp");
    expect(actions).toContain("closeOverlay");
    expect(PLAYER_SHORTCUTS.find((shortcut) => shortcut.action === "toggleShortcutHelp")?.eventKeys).toEqual(["?"]);
  });
});

function mountHelpHarness() {
  const spies = createSpies();
  const state: { helpOpen: boolean } = { helpOpen: false };

  function Harness() {
    const [helpOpen, setHelpOpen] = useState(false);
    state.helpOpen = helpOpen;
    usePlayerKeyboardShortcuts({
      itemId: "movie-1",
      volume: 0.5,
      changeVolume: spies.changeVolume,
      seekRelative: spies.seekRelative,
      togglePlayPause: spies.togglePlayPause,
      toggleMute: spies.toggleMute,
      toggleFullscreen: spies.toggleFullscreen,
      resetControlsTimeout: () => undefined,
      showInfo: false,
      showSettings: false,
      setShowInfo: spies.setShowInfo,
      setShowSettings: spies.setShowSettings,
      navigate: spies.navigate,
      shortcutHelpOpen: helpOpen,
      setShortcutHelpOpen: setHelpOpen,
    });
    return helpOpen ? <PlayerShortcutHelp onClose={() => setHelpOpen(false)} /> : null;
  }

  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  act(() => root?.render(<Harness />));
  return { spies, state, element: container };
}

function helpDialog(element: HTMLElement | undefined) {
  return element?.querySelector<HTMLElement>('[role="dialog"]') ?? null;
}

describe("player shortcut help panel", () => {
  it("opens with ? and closes with Escape", () => {
    const { state, element } = mountHelpHarness();
    expect(helpDialog(element)).toBeNull();

    keydown("?");
    expect(state.helpOpen).toBe(true);
    expect(helpDialog(element)).not.toBeNull();

    keydown("Escape");
    expect(state.helpOpen).toBe(false);
    expect(helpDialog(element)).toBeNull();
  });

  it("toggles the panel with ? a second time", () => {
    const { state } = mountHelpHarness();
    keydown("?");
    expect(state.helpOpen).toBe(true);
    keydown("?");
    expect(state.helpOpen).toBe(false);
  });

  it("swallows playback shortcuts while the panel is open", () => {
    const { spies } = mountHelpHarness();
    keydown("?");
    resetSpies(spies);

    keydown(" ");
    keydown("ArrowLeft");
    keydown("m");

    expect(spies.togglePlayPause).not.toHaveBeenCalled();
    expect(spies.seekRelative).not.toHaveBeenCalled();
    expect(spies.toggleMute).not.toHaveBeenCalled();
  });

  it("ignores ? while a text field has focus", () => {
    const { state } = mountHelpHarness();
    const input = document.createElement("input");
    document.body.append(input);
    act(() => input.focus());

    act(() => {
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "?", bubbles: true, cancelable: true }));
    });

    expect(state.helpOpen).toBe(false);
    input.remove();
  });
});

describe("PlayerShortcutHelp", () => {
  function renderHelp(onClose = vi.fn()) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    act(() => root?.render(<PlayerShortcutHelp onClose={onClose} />));
    return { onClose, element: container };
  }

  it("renders every documented shortcut as a grouped accessible dialog", () => {
    const { element } = renderHelp();
    const dialog = helpDialog(element);
    expect(dialog).not.toBeNull();
    expect(dialog?.getAttribute("aria-modal")).toBe("true");
    expect(dialog?.getAttribute("role")).toBe("dialog");
    const labelledBy = dialog?.getAttribute("aria-labelledby");
    expect(labelledBy).toBeTruthy();
    expect(dialog?.querySelector(`#${labelledBy}`)?.textContent).toBe("键盘快捷键");

    const groups = [...(element?.querySelectorAll(".lux-player-shortcut-help-group") ?? [])].map(
      (group) => group.getAttribute("aria-label"),
    );
    expect(groups).toEqual(["播放", "进度", "音量", "画面", "面板"]);

    const rows = element?.querySelectorAll(".lux-player-shortcut-help-row") ?? [];
    expect(rows).toHaveLength(PLAYER_SHORTCUTS.length);
    const text = element?.textContent ?? "";
    for (const shortcut of PLAYER_SHORTCUTS) {
      expect(text).toContain(shortcut.description);
    }
    expect(text).toContain("Esc");
    expect(element?.querySelector('button[aria-label="关闭键盘快捷键"]')).not.toBeNull();
  });

  it("closes on Escape from inside the dialog, on the close button and on an outside click", () => {
    const escapeClose = vi.fn();
    const { element } = renderHelp(escapeClose);
    const dialog = helpDialog(element);
    act(() => {
      dialog?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    expect(escapeClose).toHaveBeenCalledTimes(1);
    act(() => root?.unmount());
    root = undefined;
    container?.remove();

    const buttonClose = vi.fn();
    const rendered = renderHelp(buttonClose);
    act(() => {
      rendered.element.querySelector<HTMLButtonElement>('button[aria-label="关闭键盘快捷键"]')?.click();
    });
    expect(buttonClose).toHaveBeenCalledTimes(1);
    act(() => root?.unmount());
    root = undefined;
    container?.remove();

    const outsideClose = vi.fn();
    renderHelp(outsideClose);
    act(() => {
      document.body.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(outsideClose).toHaveBeenCalledTimes(1);

    const insideClick = vi.fn();
    act(() => root?.unmount());
    root = undefined;
    container?.remove();
    renderHelp(insideClick);
    act(() => {
      helpDialog(container)?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(insideClick).not.toHaveBeenCalled();
  });

  it("closes on a click in the surrounding layer without leaking it to the player", () => {
    const onClose = vi.fn();
    const { element } = renderHelp(onClose);
    const layer = element.querySelector<HTMLElement>(".lux-player-shortcut-help-layer");
    expect(layer).not.toBeNull();

    act(() => {
      layer?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);

    act(() => {
      helpDialog(element)?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("keeps Tab inside the dialog", () => {
    const { element } = renderHelp();
    const dialog = helpDialog(element);
    const close = element?.querySelector<HTMLButtonElement>('button[aria-label="关闭键盘快捷键"]');
    act(() => close?.focus());

    const event = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    act(() => {
      close?.dispatchEvent(event);
    });
    expect(event.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(close);
    expect(dialog?.contains(document.activeElement)).toBe(true);
  });

  it("moves focus into the dialog and restores it on unmount", () => {
    const trigger = document.createElement("button");
    trigger.textContent = "键盘快捷键";
    document.body.append(trigger);
    act(() => trigger.focus());

    const { element } = renderHelp();
    expect(document.activeElement).not.toBe(trigger);
    expect(helpDialog(element)?.contains(document.activeElement)).toBe(true);

    act(() => root?.unmount());
    root = undefined;
    expect(document.activeElement).toBe(trigger);
    trigger.remove();
  });
});
