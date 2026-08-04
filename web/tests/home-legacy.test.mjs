import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";

test("legacy home renders each library shelf and hides generic recently added", async () => {
  const dom = new JSDOM('<!doctype html><main id="app"></main>', { url: "http://localhost/" });
  globalThis.window = dom.window;
  globalThis.document = dom.window.document;

  let requestIndex = 0;
  const responses = [
    { initialized: true },
    { user: { id: "admin-1", usernameNormalized: "admin", displayName: "Admin", canManageServer: true } },
    {
      libraries: [
        {
          id: "library-1",
          name: "01每日更新",
          kind: "MOVIE",
          coverImageUrl: "/covers/daily.jpg",
          latest: [{ id: "daily-1", title: "今日新片", itemType: "MOVIE" }],
        },
        {
          id: "library-2",
          name: "02华语电影",
          kind: "MOVIE",
          coverImageUrl: "/covers/chinese.jpg",
          latest: [{ id: "chinese-1", title: "华语新片", itemType: "MOVIE" }],
        },
      ],
    },
    { libraries: [] },
  ];
  globalThis.fetch = async () => ({
    ok: true,
    status: 200,
    json: async () => responses[requestIndex++] ?? {},
  });

  await import(`../src/app.mjs?home-test=${Date.now()}`);
  await new Promise((resolve) => setTimeout(resolve, 20));

  assert.equal(document.querySelectorAll(".library-rail [data-library]").length, 2);
  assert.equal(document.querySelector(".library-rail")?.textContent.includes("01每日更新"), true);
  assert.equal(document.querySelector(".library-rail")?.textContent.includes("02华语电影"), true);
  assert.equal(document.querySelector(".library-card-cover")?.getAttribute("src"), "/covers/daily.jpg");
  const latestHeadings = [...document.querySelectorAll(".home-section h2")]
    .map((heading) => heading.textContent)
    .filter((text) => text?.startsWith("最新"));
  assert.deepEqual(latestHeadings, ["最新01每日更新", "最新02华语电影"]);
  assert.equal(document.querySelector('[data-item="daily-1"]')?.textContent.includes("今日新片"), true);
  assert.equal(document.querySelector('[data-item="chinese-1"]')?.textContent.includes("华语新片"), true);
  assert.equal(
    [...document.querySelectorAll(".home-section h2")].some((heading) => heading.textContent.includes("最近添加")),
    false,
  );
});
