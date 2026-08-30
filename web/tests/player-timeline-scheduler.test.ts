import { describe, expect, it } from "vitest";
import {
  createPlaybackTimelineScheduler,
  type PlaybackTimelineSnapshot,
} from "../src/features/player/player-timeline-scheduler";

describe("createPlaybackTimelineScheduler", () => {
  it("coalesces multiple snapshots queued before the next animation frame", () => {
    const callbacks: Array<() => void> = [];
    const updates: PlaybackTimelineSnapshot[] = [];
    const scheduler = createPlaybackTimelineScheduler(
      (snapshot) => updates.push(snapshot),
      (callback) => {
        callbacks.push(callback);
        return callbacks.length;
      },
      () => undefined,
    );

    scheduler.schedule({ currentTime: 1, duration: 100, bufferedEnd: 4 });
    scheduler.schedule({ currentTime: 2, duration: 100, bufferedEnd: 5 });

    expect(callbacks).toHaveLength(1);
    callbacks[0]();
    expect(updates).toEqual([{ currentTime: 2, duration: 100, bufferedEnd: 5 }]);

    scheduler.dispose();
  });

  it("flushes immediately when a timeline event needs synchronous UI state", () => {
    const callbacks: Array<() => void> = [];
    const updates: PlaybackTimelineSnapshot[] = [];
    const scheduler = createPlaybackTimelineScheduler(
      (snapshot) => updates.push(snapshot),
      (callback) => {
        callbacks.push(callback);
        return callbacks.length;
      },
      () => undefined,
    );

    scheduler.schedule({ currentTime: 8, duration: 100, bufferedEnd: 10 });
    scheduler.schedule({ currentTime: 9, duration: 100, bufferedEnd: 11 }, true);

    expect(updates).toEqual([{ currentTime: 9, duration: 100, bufferedEnd: 11 }]);
    expect(callbacks).toHaveLength(1);
    scheduler.dispose();
  });
});
