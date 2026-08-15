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
    document.title = "Lux";
  });

  it("uses the server name as the default browser tab title", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LuxShell
            user={{ id: "user-1", usernameNormalized: "test" }}
            serverName="客厅 Lux"
          />
        </MemoryRouter>,
      );
    });

    expect(document.title).toBe("客厅 Lux - Lux");
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

  it("renders black and white project logo variants in the brand link", () => {
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

    expect(logo?.querySelector<HTMLImageElement>(".lux-theme-logo-light")?.getAttribute("src")).toBe("/logo-black.svg");
    expect(logo?.querySelector<HTMLImageElement>(".lux-theme-logo-dark")?.getAttribute("src")).toBe("/logo-white.svg");
    expect(logo?.querySelector<HTMLImageElement>(".lux-theme-logo-light")?.getAttribute("alt")).toBe("");
  });

  it("keeps the light theme mapped to the black logo variant", () => {
    document.documentElement.dataset.luxTheme = "light";
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
    expect(logo?.querySelector(".lux-theme-logo-light")).toBeTruthy();
    expect(logo?.querySelector(".lux-theme-logo-dark")).toBeTruthy();
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

  it("exposes the user's favorites in desktop and mobile navigation", () => {
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
            }}
          />
        </MemoryRouter>,
      );
    });

    expect(container.querySelector('.lux-desktop-nav a[href="/favorites"]')?.textContent).toBe("收藏");
    act(() => container.querySelector<HTMLButtonElement>(".lux-menu-button")?.click());
    expect(container.querySelector('.lux-mobile-nav a[href="/favorites"]')?.textContent?.trim()).toBe("收藏");
  });

  it("keeps mobile navigation rows consistent and touch-sized", () => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);

    act(() => {
      root.render(
        <MemoryRouter>
          <LuxShell user={{ id: "user-1", usernameNormalized: "test", canManageServer: true }} />
        </MemoryRouter>,
      );
    });
    act(() => container.querySelector<HTMLButtonElement>(".lux-menu-button")?.click());

    const links = Array.from(container.querySelectorAll<HTMLAnchorElement>(".lux-mobile-nav-link"));
    expect(links).toHaveLength(5);
    expect(links.every((link) => link.querySelector(".lux-mobile-nav-icon svg") && link.querySelector(".lux-mobile-nav-label")?.textContent?.trim())).toBe(true);
    expect(links.every((link) => link.querySelectorAll(".lux-mobile-nav-icon").length === 1)).toBe(true);
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
