// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";

const metadataEditorModule = vi.hoisted(() => ({ loaded: false, rendered: false }));

vi.mock("../src/features/media/MediaMetadataEditor", () => {
  metadataEditorModule.loaded = true;
  return {
    MediaMetadataEditor: function MetadataEditorMock() {
      metadataEditorModule.rendered = true;
      return null;
    },
  };
});

import { MediaCard } from "../src/features/home/media";

const metadataEditorLoadedBeforeFirstRender = metadataEditorModule.loaded;

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("MediaCard editor loading", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("loads the metadata editor only after its menu action is selected", async () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <MediaCard item={{ id: "item-1", title: "二毛", itemType: "MOVIE" }} />
        </MemoryRouter>,
      );
    });

    expect(metadataEditorLoadedBeforeFirstRender).toBe(false);
    expect(metadataEditorModule.rendered).toBe(false);

    await act(async () => {
      container.querySelector<HTMLButtonElement>(".lux-media-actions-trigger")?.click();
    });
    await act(async () => {
      document.body.querySelector<HTMLButtonElement>('[data-action="edit-metadata"]')?.click();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(metadataEditorModule.loaded).toBe(true);
    expect(metadataEditorModule.rendered).toBe(true);
  });
});
