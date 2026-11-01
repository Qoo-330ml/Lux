import { X } from "lucide-react";
import { useEffect, useRef, type KeyboardEvent as ReactKeyboardEvent } from "react";
import {
  PLAYER_SHORTCUTS,
  type PlayerShortcut,
  type PlayerShortcutGroup,
} from "../usePlayerKeyboardShortcuts";

const TITLE_ID = "lux-player-shortcut-help-title";

const FOCUSABLE_SELECTOR =
  "button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";

export type PlayerShortcutHelpProps = {
  onClose: () => void;
  /** 默认使用全局快捷键数据源；测试或局部帮助可传入子集。 */
  shortcuts?: readonly PlayerShortcut[];
};

type ShortcutGroup = {
  title: PlayerShortcutGroup;
  shortcuts: PlayerShortcut[];
};

/** 按数据源的出现顺序分组，面板顺序与处理器登记顺序保持一致。 */
export function groupPlayerShortcuts(shortcuts: readonly PlayerShortcut[]): ShortcutGroup[] {
  const groups: ShortcutGroup[] = [];
  for (const shortcut of shortcuts) {
    const existing = groups.find((group) => group.title === shortcut.group);
    if (existing) {
      existing.shortcuts.push(shortcut);
    } else {
      groups.push({ title: shortcut.group, shortcuts: [shortcut] });
    }
  }
  return groups;
}

/**
 * The player's keyboard shortcut reference.
 *
 * It is a modal dialog: focus moves in on open, Tab cycles inside, Escape or an
 * outside click closes it, and the previously focused control is restored. The
 * rows come from PLAYER_SHORTCUTS, the same list the key handler dispatches on,
 * so the panel can never document a shortcut that does not work.
 */
export function PlayerShortcutHelp({ onClose, shortcuts = PLAYER_SHORTCUTS }: PlayerShortcutHelpProps) {
  const layerRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  // PlayerPage passes a fresh inline callback on every render; keeping it in a
  // ref lets the mount effect own focus for exactly one open/close cycle.
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    panelRef.current?.focus();

    const handleClick = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      // The layer itself owns its own clicks; only clicks that bypass it (portals,
      // dialogs stacked above) reach this listener.
      if (layerRef.current?.contains(target)) return;
      // The control-bar entry point toggles the panel itself; without this the
      // open click would close and immediately reopen the dialog.
      if (target.closest("[data-player-shortcut-help-toggle]")) return;
      onCloseRef.current();
    };

    document.addEventListener("click", handleClick);
    return () => {
      document.removeEventListener("click", handleClick);
      previous?.focus();
    };
  }, []);

  const handleKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onCloseRef.current();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = panelRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR);
    if (!focusable || focusable.length === 0) {
      event.preventDefault();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;
    if (event.shiftKey && (active === first || active === panelRef.current)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  };

  return (
    <div
      ref={layerRef}
      className="lux-player-shortcut-help-layer"
      onClick={(event) => {
        // 点击浮层外部（视频、控制条等）只关闭浮层，不把点击继续交给播放器。
        event.stopPropagation();
        if (event.target === event.currentTarget) onCloseRef.current();
      }}
      // 双击关闭时，两次点击都停在遮罩上，不能落到视频的播放/全屏处理器。
      onDoubleClick={(event) => event.stopPropagation()}
      // 右键不切换遮罩背后的播放信息面板。
      onContextMenu={(event) => event.stopPropagation()}
    >
      <div
        ref={panelRef}
        className="lux-player-shortcut-help"
        role="dialog"
        aria-modal="true"
        aria-labelledby={TITLE_ID}
        tabIndex={-1}
        data-player-shortcut-help
        onClick={(event) => event.stopPropagation()}
        onDoubleClick={(event) => event.stopPropagation()}
        onKeyDown={handleKeyDown}
      >
        <header className="lux-player-shortcut-help-head">
          <div>
            <h2 id={TITLE_ID}>键盘快捷键</h2>
            <p className="lux-player-shortcut-help-hint">播放器页面内生效；按 Esc 关闭。</p>
          </div>
          <button
            type="button"
            className="lux-player-shortcut-help-close"
            aria-label="关闭键盘快捷键"
            title="关闭 (Esc)"
            onClick={() => onCloseRef.current()}
          >
            <X size={16} aria-hidden="true" />
          </button>
        </header>
        <div className="lux-player-shortcut-help-body">
          {groupPlayerShortcuts(shortcuts).map((group) => (
            <section
              key={group.title}
              className="lux-player-shortcut-help-group"
              aria-label={group.title}
            >
              <h3>{group.title}</h3>
              <dl>
                {group.shortcuts.map((shortcut) => (
                  <div key={shortcut.action} className="lux-player-shortcut-help-row">
                    <dt>
                      {shortcut.keys.map((key) => (
                        <kbd key={key}>{key}</kbd>
                      ))}
                    </dt>
                    <dd>{shortcut.description}</dd>
                  </div>
                ))}
              </dl>
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}
