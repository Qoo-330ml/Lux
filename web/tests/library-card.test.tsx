// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it } from "vitest";
import { LibraryCard } from "../src/features/home/media";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("LibraryCard", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("uses the configured cover image when available", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LibraryCard
            library={{
              id: "library-1",
              name: "电影",
              kind: "MOVIE",
              coverImageUrl: "/api/v1/libraries/library-1/cover",
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector<HTMLImageElement>(".lux-library-cover")?.src).toContain(
      "/api/v1/libraries/library-1/cover",
    );
    expect(container.querySelector(".lux-library-icon")).toBeNull();
  });
});
