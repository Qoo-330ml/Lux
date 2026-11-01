import { useEffect, useRef } from "react";

/**
 * The single source of truth for the player's keyboard shortcuts.
 *
 * The key handler dispatches on `action` and the help panel renders `group`,
 * `keys` and `description`, so the documentation cannot drift from behaviour:
 * adding a shortcut here both binds it and shows it in the panel.
 */
export type PlayerShortcutAction =
  | "togglePlayPause"
  | "seekBackward"
  | "seekForward"
  | "volumeUp"
  | "volumeDown"
  | "toggleInfo"
  | "toggleMute"
  | "toggleFullscreen"
  | "toggleShortcutHelp"
  | "closeOverlay";

export type PlayerShortcutGroup = "播放" | "进度" | "音量" | "画面" | "面板";

export type PlayerShortcut = {
  action: PlayerShortcutAction;
  group: PlayerShortcutGroup;
  /** 帮助面板里展示的按键标签。 */
  keys: readonly string[];
  /** 与 KeyboardEvent.key 逐一匹配的取值；字母同时登记大小写，保持原有行为。 */
  eventKeys: readonly string[];
  description: string;
};

export const PLAYER_SHORTCUTS: readonly PlayerShortcut[] = [
  {
    action: "togglePlayPause",
    group: "播放",
    keys: ["空格", "K"],
    eventKeys: [" ", "k", "K"],
    description: "播放 / 暂停",
  },
  {
    action: "seekBackward",
    group: "进度",
    keys: ["←", "J"],
    eventKeys: ["ArrowLeft", "j", "J"],
    description: "后退 10 秒",
  },
  {
    action: "seekForward",
    group: "进度",
    keys: ["→", "L"],
    eventKeys: ["ArrowRight", "l", "L"],
    description: "前进 10 秒",
  },
  {
    action: "volumeUp",
    group: "音量",
    keys: ["↑"],
    eventKeys: ["ArrowUp"],
    description: "音量 +5%",
  },
  {
    action: "volumeDown",
    group: "音量",
    keys: ["↓"],
    eventKeys: ["ArrowDown"],
    description: "音量 −5%",
  },
  {
    action: "toggleMute",
    group: "音量",
    keys: ["M"],
    eventKeys: ["m", "M"],
    description: "静音 / 取消静音",
  },
  {
    action: "toggleFullscreen",
    group: "画面",
    keys: ["F"],
    eventKeys: ["f", "F"],
    description: "进入 / 退出全屏",
  },
  {
    action: "toggleInfo",
    group: "面板",
    keys: ["I"],
    eventKeys: ["i", "I"],
    description: "显示 / 隐藏播放信息",
  },
  {
    action: "toggleShortcutHelp",
    group: "面板",
    keys: ["?"],
    eventKeys: ["?"],
    description: "显示 / 隐藏键盘快捷键",
  },
  {
    action: "closeOverlay",
    group: "面板",
    keys: ["Esc"],
    eventKeys: ["Escape"],
    description: "关闭当前浮层；没有浮层时返回上一页",
  },
];

const actionByEventKey = new Map<string, PlayerShortcutAction>();
for (const shortcut of PLAYER_SHORTCUTS) {
  for (const eventKey of shortcut.eventKeys) actionByEventKey.set(eventKey, shortcut.action);
}

/** 把 KeyboardEvent.key 解析为数据源登记的动作；未登记返回 null。 */
export function playerShortcutActionForEventKey(key: string): PlayerShortcutAction | null {
  return actionByEventKey.get(key) ?? null;
}

type PlayerKeyboardShortcutOptions = {
  itemId: string;
  volume: number;
  changeVolume: (newVol: number) => void;
  seekRelative: (seconds: number) => void;
  togglePlayPause: () => void;
  toggleMute: () => void;
  toggleFullscreen: () => void;
  resetControlsTimeout: () => void;
  showInfo: boolean;
  showSettings: boolean;
  setShowInfo: (value: boolean | ((visible: boolean) => boolean)) => void;
  setShowSettings: (value: boolean | ((visible: boolean) => boolean)) => void;
  navigate: (to: number | string) => void;
  /** 帮助浮层打开时其余快捷键让位，避免在浮层后面误触发播放。 */
  shortcutHelpOpen?: boolean;
  setShortcutHelpOpen?: (value: boolean | ((open: boolean) => boolean)) => void;
};

/**
 * Global player shortcuts. Typing targets keep their own key handling, and
 * Escape unwinds overlays before it ever navigates away.
 *
 * The listener is bound once per mount and reads the latest callbacks through a
 * ref, mirroring usePlayerPlatform: playback re-renders the page roughly ten
 * times per second, so depending on callback identity here would detach and
 * reattach the window listener on every tick.
 */
export function usePlayerKeyboardShortcuts(options: PlayerKeyboardShortcutOptions) {
  const optionsRef = useRef(options);
  optionsRef.current = options;

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable)
      ) {
        return;
      }

      const {
        itemId,
        navigate,
        resetControlsTimeout,
        setShowInfo,
        setShowSettings,
        setShortcutHelpOpen,
        shortcutHelpOpen,
        showInfo,
        showSettings,
        volume,
        changeVolume,
        seekRelative,
        toggleFullscreen,
        toggleMute,
        togglePlayPause,
      } = optionsRef.current;

      resetControlsTimeout();

      const action = playerShortcutActionForEventKey(e.key);
      if (!action) return;

      // The help panel is modal: while it is open only Escape acts, and every
      // other player shortcut is swallowed instead of reaching the video.
      if (shortcutHelpOpen) {
        e.preventDefault();
        if (action === "closeOverlay") setShortcutHelpOpen?.(false);
        return;
      }

      switch (action) {
        case "togglePlayPause":
          e.preventDefault();
          togglePlayPause();
          break;
        case "seekBackward":
          e.preventDefault();
          seekRelative(-10);
          break;
        case "seekForward":
          e.preventDefault();
          seekRelative(10);
          break;
        case "volumeUp":
          e.preventDefault();
          changeVolume(volume + 0.05);
          break;
        case "volumeDown":
          e.preventDefault();
          changeVolume(volume - 0.05);
          break;
        case "toggleInfo":
          e.preventDefault();
          setShowInfo((visible) => !visible);
          break;
        case "toggleMute":
          e.preventDefault();
          toggleMute();
          break;
        case "toggleFullscreen":
          e.preventDefault();
          toggleFullscreen();
          break;
        case "toggleShortcutHelp":
          e.preventDefault();
          setShortcutHelpOpen?.((open) => !open);
          break;
        case "closeOverlay":
          if (showInfo) {
            e.preventDefault();
            setShowInfo(false);
          } else if (showSettings) {
            e.preventDefault();
            setShowSettings(false);
          } else if (!document.fullscreenElement) {
            e.preventDefault();
            if (window.history.length > 1) {
              navigate(-1);
            } else {
              navigate(itemId ? `/items/${encodeURIComponent(itemId)}` : "/");
            }
          }
          break;
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, []);
}
