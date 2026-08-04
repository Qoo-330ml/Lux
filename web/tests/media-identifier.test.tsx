// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaIdentifier } from "../src/features/media/MediaIdentifier";
import { api } from "../src/lib/api/client";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("MediaIdentifier", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.restoreAllMocks();
  });

  it("searches candidates and applies the selected metadata match", async () => {
    vi.spyOn(api, "adminItemCandidates").mockResolvedValue({ items: [], total: 0 });
    const search = vi.spyOn(api, "searchAdminItemCandidates").mockResolvedValue({
      items: [{
        id: "candidate-1",
        itemId: "item-1",
        itemTitle: "示例电影",
        provider: "TMDB",
        providerId: "123",
        candidate: { title: "候选电影", originalTitle: "Candidate Movie", productionYear: 2020, overview: "候选简介" },
        score: 86,
        status: "PENDING",
        fieldDiffs: [{ field: "标题", current: "示例电影", candidate: "候选电影" }],
      }],
      total: 1,
    });
    const select = vi.spyOn(api, "selectAdminMetadata").mockResolvedValue({
      itemId: "item-1",
      candidateId: "candidate-1",
      status: "ONLINE_CONFIRMED",
    });
    const onClose = vi.fn();
    const onSaved = vi.fn();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    await act(async () => {
      root.render(<MediaIdentifier item={{ id: "item-1", title: "示例电影", productionYear: 2020 }} onClose={onClose} onSaved={onSaved} />);
    });
    await act(async () => await Promise.resolve());

    expect(container.querySelector<HTMLInputElement>("#identify-query")?.value).toBe("示例电影");
    await act(async () => container.querySelector<HTMLButtonElement>("[data-action=identify-search]")?.click());
    expect(search).toHaveBeenCalledWith("item-1", "示例电影", 2020);
    expect(container.textContent).toContain("候选电影");
    expect(container.textContent).toContain("86 分");

    await act(async () => container.querySelector<HTMLButtonElement>("[data-action=identify-fill]")?.click());
    expect(select).toHaveBeenCalledWith("item-1", "candidate-1", "fillMissing");
    expect(onSaved).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });
});
