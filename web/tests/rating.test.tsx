// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { Rating } from "../src/features/home/media";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("Rating", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("shows the scraper source and score on the rating badge", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(<Rating value={8.6} source="TMDb" />);
    });

    const badge = container.querySelector(".lux-rating");
    expect(badge?.textContent).toContain("TMDb");
    expect(badge?.textContent).toContain("8.6");
    expect(badge?.getAttribute("aria-label")).toBe("TMDb 评分 8.6");
  });

  it("does not render for a missing or invalid score", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(<Rating value={null} source="TMDb" />);
    });

    expect(container.querySelector(".lux-rating")).toBeNull();
  });
});
