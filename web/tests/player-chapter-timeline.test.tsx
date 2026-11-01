// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  PlayerChapterTimeline,
  PlayerIntroSkip,
  PlayerSkipCredits,
} from "../src/features/player/components/player-chapter-timeline";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LuxPlayer chapter controls", () => {
  let container: HTMLDivElement | undefined;
  let root: Root | undefined;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    container?.remove();
  });

  it("provides focusable titled segments that seek to the segment start", async () => {
    const onSeek = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => root?.render(
      <PlayerChapterTimeline
        duration={100}
        segments={[{
          id: "chapter-1",
          start: 25,
          end: 75,
          title: "中段",
          markerType: "CHAPTER",
          chapterIndex: 0,
        }]}
        onSeek={onSeek}
      />,
    ));

    const segment = container.querySelector<HTMLButtonElement>("[aria-label='章节：中段']");
    expect(segment).not.toBeNull();
    expect(segment?.getAttribute("title")).toContain("00:25");
    expect(segment?.style.width).toBe("4px");
    await act(async () => segment?.click());
    expect(onSeek).toHaveBeenCalledWith(25);
  });

  it("only shows skip intro while the current time is inside the explicit range", async () => {
    const onSkip = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => root?.render(
      <PlayerIntroSkip currentTime={3} introSkip={{ start: 2, end: 6 }} onSkip={onSkip} />,
    ));
    const skip = container.querySelector<HTMLButtonElement>("[aria-label='跳过片头']");
    expect(skip).not.toBeNull();
    await act(async () => skip?.click());
    expect(onSkip).toHaveBeenCalledWith(6);

    await act(async () => root?.render(
      <PlayerIntroSkip currentTime={6} introSkip={{ start: 2, end: 6 }} onSkip={onSkip} />,
    ));
    expect(container.querySelector("[aria-label='跳过片头']")).toBeNull();
  });

  it("only offers skip credits once the credits chapter starts", async () => {
    const onSkip = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => root?.render(
      <PlayerSkipCredits
        currentTime={89}
        duration={120}
        creditsStart={90}
        hasNextEpisode
        onSkip={onSkip}
      />,
    ));
    expect(container.querySelector("[aria-label='跳过片尾']")).toBeNull();

    await act(async () => root?.render(
      <PlayerSkipCredits
        currentTime={91}
        duration={120}
        creditsStart={90}
        hasNextEpisode
        onSkip={onSkip}
      />,
    ));
    const skip = container.querySelector<HTMLButtonElement>("[aria-label='跳过片尾']");
    expect(skip).not.toBeNull();
    expect(skip?.textContent).toBe("跳过片尾");
    await act(async () => skip?.click());
    expect(onSkip).toHaveBeenCalledTimes(1);
  });

  it("hides skip credits without a credits marker or after the media ended", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => root?.render(
      <PlayerSkipCredits
        currentTime={95}
        duration={120}
        creditsStart={null}
        hasNextEpisode={false}
        onSkip={() => undefined}
      />,
    ));
    expect(container.querySelector("[aria-label='跳过片尾']")).toBeNull();

    await act(async () => root?.render(
      <PlayerSkipCredits
        currentTime={120}
        duration={120}
        creditsStart={90}
        hasNextEpisode={false}
        onSkip={() => undefined}
      />,
    ));
    expect(container.querySelector("[aria-label='跳过片尾']")).toBeNull();
  });
});
