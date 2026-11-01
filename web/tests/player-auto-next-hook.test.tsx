// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useAutoPlayNext } from "../src/features/player/useAutoPlayNext";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

type ProbeProps = {
  enabled?: boolean;
  nextEpisodeId?: string | null;
  playbackKey?: string;
  onNavigateNextEpisode?: (episodeId: string) => void;
};

function Probe({
  enabled = true,
  nextEpisodeId = "episode-2",
  playbackKey = "episode-1:default",
  onNavigateNextEpisode = () => undefined,
}: ProbeProps) {
  const { countdown, handleEnded, playNow, cancel } = useAutoPlayNext({
    playbackKey,
    nextEpisodeId,
    enabled,
    onNavigateNextEpisode,
  });
  return (
    <>
      <button type="button" data-testid="ended" onClick={handleEnded}>结束</button>
      <button type="button" data-testid="play-now" onClick={playNow}>立即播放</button>
      <button type="button" data-testid="cancel" onClick={cancel}>取消</button>
      <output data-testid="countdown">{countdown ? countdown.secondsLeft : "hidden"}</output>
    </>
  );
}

describe("LuxPlayer auto play next wiring", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    vi.useRealTimers();
    if (root) act(() => root?.unmount());
    container?.remove();
    root = undefined;
  });

  async function render(props: ProbeProps = {}) {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    await act(async () => root?.render(<Probe {...props} />));
    return {
      ended: () => container?.querySelector<HTMLButtonElement>("[data-testid='ended']"),
      playNow: () => container?.querySelector<HTMLButtonElement>("[data-testid='play-now']"),
      cancel: () => container?.querySelector<HTMLButtonElement>("[data-testid='cancel']"),
      countdown: () => container?.querySelector("[data-testid='countdown']")?.textContent,
    };
  }

  it("counts down once per second and navigates when it reaches zero", async () => {
    vi.useFakeTimers();
    const onNavigate = vi.fn();
    const ui = await render({ onNavigateNextEpisode: onNavigate });

    await act(async () => ui.ended()?.click());
    expect(ui.countdown()).toBe("10");

    await act(async () => vi.advanceTimersByTime(1000));
    expect(ui.countdown()).toBe("9");

    await act(async () => vi.advanceTimersByTime(9000));
    expect(ui.countdown()).toBe("hidden");
    expect(onNavigate).toHaveBeenCalledTimes(1);
    expect(onNavigate).toHaveBeenCalledWith("episode-2");

    await act(async () => vi.advanceTimersByTime(5000));
    expect(onNavigate).toHaveBeenCalledTimes(1);
  });

  it("navigates immediately from the card", async () => {
    const onNavigate = vi.fn();
    const ui = await render({ onNavigateNextEpisode: onNavigate });

    await act(async () => ui.ended()?.click());
    await act(async () => ui.playNow()?.click());

    expect(ui.countdown()).toBe("hidden");
    expect(onNavigate).toHaveBeenCalledWith("episode-2");
  });

  it("does not offer the card when the preference is off or no next episode exists", async () => {
    const onNavigate = vi.fn();
    const disabled = await render({ enabled: false, onNavigateNextEpisode: onNavigate });
    await act(async () => disabled.ended()?.click());
    expect(disabled.countdown()).toBe("hidden");

    await act(async () => root?.unmount());
    root = undefined;
    const last = await render({ nextEpisodeId: null, onNavigateNextEpisode: onNavigate });
    await act(async () => last.ended()?.click());
    expect(last.countdown()).toBe("hidden");
    expect(onNavigate).not.toHaveBeenCalled();
  });

  it("keeps a cancelled card closed and resets when the episode changes", async () => {
    vi.useFakeTimers();
    const onNavigate = vi.fn();
    const ui = await render({ onNavigateNextEpisode: onNavigate });

    await act(async () => ui.ended()?.click());
    await act(async () => ui.cancel()?.click());
    expect(ui.countdown()).toBe("hidden");

    await act(async () => vi.advanceTimersByTime(20_000));
    expect(onNavigate).not.toHaveBeenCalled();

    await act(async () => root?.render(<Probe playbackKey="episode-2:default" onNavigateNextEpisode={onNavigate} />));
    await act(async () => ui.ended()?.click());
    expect(ui.countdown()).toBe("10");
  });

  it("clears a running countdown when the preference is switched off", async () => {
    const ui = await render();
    await act(async () => ui.ended()?.click());
    expect(ui.countdown()).toBe("10");

    await act(async () => root?.render(<Probe enabled={false} />));
    expect(ui.countdown()).toBe("hidden");
  });
});
