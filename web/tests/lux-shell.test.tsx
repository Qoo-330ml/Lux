// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, describe, expect, it } from "vitest";
import { LuxShell, useAvatar } from "../src/components/layout/LuxShell";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function AvatarUpdateFixture() {
  const { setAvatarUrl } = useAvatar();

  return <button type="button" onClick={() => setAvatarUrl("/api/v1/auth/avatar?v=updated")}>更新头像</button>;
}

describe("LuxShell user control", () => {
  let container: HTMLDivElement;
  let root: Root;

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("renders the server avatar and falls back to initials when it is unavailable", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    const userButton = container.querySelector<HTMLButtonElement>(".lux-user-button");

    expect(userButton?.querySelector<HTMLImageElement>(".lux-avatar img")?.getAttribute("src")).toBe(
      "/api/v1/auth/avatar",
    );
    act(() => {
      userButton?.querySelector<HTMLImageElement>(".lux-avatar img")?.dispatchEvent(new Event("error"));
    });
    expect(userButton?.querySelector(".lux-avatar")?.textContent).toBe("T");
    expect(userButton?.querySelector(".lux-user-label")).toBeNull();
    expect(userButton?.textContent).toBe("T");
  });

  it("updates the header avatar when the account avatar changes", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter initialEntries={["/"]}>
          <Routes>
            <Route
              element={
                <LuxShell
                  user={{
                    id: "user-1",
                    usernameNormalized: "test",
                    displayName: "test",
                  }}
                />
              }
            >
              <Route index element={<AvatarUpdateFixture />} />
            </Route>
          </Routes>
        </MemoryRouter>,
      );
    });

    act(() => {
      Array.from(container.querySelectorAll<HTMLButtonElement>('button[type="button"]'))
        .find((button) => button.textContent === "更新头像")
        ?.click();
    });

    expect(container.querySelector<HTMLImageElement>(".lux-avatar img")?.getAttribute("src")).toBe(
      "/api/v1/auth/avatar?v=updated",
    );
  });

  it("renders the project logo in the brand link", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    const logo = container.querySelector<HTMLImageElement>(".lux-brand-logo");

    expect(logo?.getAttribute("src")).toBe("/logo.svg");
    expect(logo?.getAttribute("alt")).toBe("");
  });

  it("does not render duplicate search or library actions in the header", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector('[aria-label="搜索"]')).toBeNull();
    expect(container.querySelector(".lux-grid-button")).toBeNull();
  });

  it("hides the back button on the home page", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter initialEntries={["/"]}>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector(".lux-back-button")).toBeNull();
    expect(container.querySelector(".lux-app.is-home-route")).toBeTruthy();
  });

  it("hides the back button on nested pages too", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter initialEntries={["/items/item-1"]}>
          <LuxShell
            user={{
              id: "user-1",
              usernameNormalized: "test",
              displayName: "test",
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector(".lux-back-button")).toBeNull();
  });
});
