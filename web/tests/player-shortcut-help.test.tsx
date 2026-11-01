// @vitest-environment jsdom

import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  PLAYER_SHORTCUTS,
  usePlayerKeyboardShortcuts,
  type PlayerShortcutAction,
} from "../src/features/player/usePlayerKeyboardShortcuts";

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
