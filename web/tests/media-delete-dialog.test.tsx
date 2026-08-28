// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { MediaDeleteDialog } from "../src/features/media/MediaDeleteDialog";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("MediaDeleteDialog", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("warns that deleting a series removes every episode", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MediaDeleteDialog
          item={{ id: "series-1", title: "示例剧集", itemType: "SERIES" }}
          onClose={() => undefined}
          onConfirm={async () => undefined}
        />,
      );
    });

    expect(container.textContent).toContain("整部剧及所有分集");
    expect(container.textContent).toContain("所有季度和分集的视频文件");
  });

  it("keeps the current-version warning for a single media item", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MediaDeleteDialog
          item={{ id: "movie-1", title: "示例电影", itemType: "MOVIE" }}
          onClose={() => undefined}
          onConfirm={async () => undefined}
        />,
      );
    });

    expect(container.textContent).toContain("当前视频版本");
    expect(container.textContent).not.toContain("所有季度和分集的视频文件");
  });
});
