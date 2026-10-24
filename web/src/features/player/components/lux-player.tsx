import type { MouseEvent, ReactNode, RefObject } from "react";

type LuxPlayerProps = {
  controlsVisible: boolean;
  containerRef: RefObject<HTMLElement | null>;
  onActivity: () => void;
  /** Right-click inside the player opens the information panel instead of the browser menu. */
  onContextMenu?: (event: MouseEvent<HTMLElement>) => void;
  surface?: ReactNode;
  topBar?: ReactNode;
  settings?: ReactNode;
  controls?: ReactNode;
  captionSlot?: ReactNode;
  danmuSlot?: ReactNode;
  children?: ReactNode;
};

/**
 * LuxPlayer's presentation boundary. Playback sessions and engines stay in
 * PlayerPage; this component only owns the player surface composition.
 */
export function LuxPlayer({
  controlsVisible,
  containerRef,
  onActivity,
  onContextMenu,
  surface,
  topBar,
  settings,
  controls,
  captionSlot,
  danmuSlot,
  children,
}: LuxPlayerProps) {
  return (
    <main
      ref={containerRef}
      className={`lux-player-page ${controlsVisible ? "controls-visible" : "controls-hidden"}`}
      onMouseMove={onActivity}
      onTouchStart={onActivity}
      onClick={onActivity}
      onContextMenu={onContextMenu}
    >
      {children ?? (
        <>
          {surface}
          <div className="lux-player-vignette-top" aria-hidden="true" />
          <div className="lux-player-vignette-bottom" aria-hidden="true" />
          {topBar}
          {settings}
          {captionSlot}
          {danmuSlot}
          {controls}
        </>
      )}
    </main>
  );
}
