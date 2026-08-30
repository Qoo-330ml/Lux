import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("detail copy starts at the poster top with the logo above its title", () => {
  const source = readFileSync(new URL("../src/features/detail/MediaDetailPage.tsx", import.meta.url), "utf8");
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const titleRowStart = source.indexOf('className="lux-detail-title-row"');
  const titleRowEnd = source.indexOf("</div>", titleRowStart);
  const titleRow = titleRowStart >= 0 && titleRowEnd > titleRowStart ? source.slice(titleRowStart, titleRowEnd) : "";
  const detailGridRule = stylesheet.match(/\.lux-detail-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.ok(titleRow.includes('className="lux-detail-logo"'), "the logo should be in the title row");
  assert.ok(titleRow.indexOf('className="lux-detail-logo"') < titleRow.indexOf("<h1>"), "the logo should precede the title");
  assert.match(detailGridRule, /align-items:\s*start/);
});

test("detail actions leave space below the overview", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const actionRule = stylesheet.match(/\.lux-detail-copy\s*>\s*\.lux-hero-actions\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(actionRule, /margin-top:\s*24px/);
});

test("mobile detail hero keeps the immersive poster compact", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const posterRule = mobileStyles.match(/\.lux-detail-poster\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(posterRule, /height:\s*clamp\(384px,\s*120vw,\s*520px\)/);
  assert.match(posterRule, /aspect-ratio:\s*auto/);
  assert.match(posterRule, /border:\s*0/);
  assert.match(posterRule, /border-radius:\s*0/);
});

test("mobile detail poster fades broadly into the content surface", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const posterFadeRule = mobileStyles.match(/\.lux-detail-poster::after\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(posterFadeRule, /transparent\s+40%/);
  assert.match(posterFadeRule, /rgba\(5,5,6,\.04\)\s+54%/);
  assert.match(posterFadeRule, /rgba\(5,5,6,\.32\)\s+70%/);
  assert.match(posterFadeRule, /rgba\(5,5,6,\.72\)\s+86%/);
  assert.match(posterFadeRule, /#050506\s+100%/);
});

test("detail grid fills the container so left and right outer spacing match", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const detailGridRule = stylesheet.match(/\.lux-detail-grid\s*\{([^}]*)\}/)?.[1] ?? "";
  const episodeGridRule = stylesheet.match(/\.lux-detail-page-episode \.lux-detail-grid\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(detailGridRule, /grid-template-columns:\s*250px\s+minmax\(0,\s*1fr\)/);
  assert.match(episodeGridRule, /grid-template-columns:\s*minmax\(320px,\s*420px\)\s+minmax\(0,\s*1fr\)/);
});

test("active React pages do not render global back controls", () => {
  const sources = [
    "../src/features/detail/MediaDetailPage.tsx",
    "../src/features/player/PlayerPage.tsx",
    "../src/features/admin/AdminLayout.tsx",
  ].map((path) => readFileSync(new URL(path, import.meta.url), "utf8")).join("\n");

  assert.doesNotMatch(sources, /className="lux-detail-back"|lux-player-topbar[^\n]*返回媒体详情|className="lux-admin-back"/);
});

test("detail page uses a compact title and a three-line expandable overview", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");

  assert.match(stylesheet, /\.lux-detail-copy h1\s*\{[^}]*font-size:\s*28px/);
  assert.match(stylesheet, /\.lux-detail-overview-text\s*\{[^}]*max-height:\s*calc\(1\.7em\s*\*\s*3\)/);
  assert.match(stylesheet, /\.lux-detail-overview-more\.is-underlined\s*\{[^}]*text-decoration:\s*underline/);
});

test("detail copy stretches its overview to the available right edge", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const overviewRule = stylesheet.match(/\.lux-detail-overview\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(overviewRule, /max-width:\s*none/);
});

test("media stream cards keep a compact width on wide detail pages", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const streamGridRule = stylesheet.match(/\.lux-media-stream-grid\s*\{([^}]*)\}/)?.[1] ?? "";
  const streamCardRule = stylesheet.match(/\.lux-media-stream-card\s*\{([^}]*)\}/)?.[1] ?? "";
  const scrollViewportRule = stylesheet.match(/^\.lux-horizontal-scroll-viewport\s*\{([^}]*)\}/m)?.[1] ?? "";

  assert.match(streamGridRule, /display:\s*flex/);
  assert.match(streamGridRule, /flex-wrap:\s*nowrap/);
  assert.match(scrollViewportRule, /overflow-x:\s*auto/);
  assert.match(streamCardRule, /flex:\s*0\s*0\s*260px/);
});

test("shared horizontal rails hide native scrollbars while preserving horizontal scrolling", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const scrollViewportRule = stylesheet.match(/^\.lux-horizontal-scroll-viewport\s*\{([^}]*)\}/m)?.[1] ?? "";
  const webkitScrollbarRule = stylesheet.match(/\.lux-horizontal-scroll-viewport::\-webkit-scrollbar\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(scrollViewportRule, /overflow-x:\s*auto/);
  assert.match(scrollViewportRule, /scrollbar-width:\s*none/);
  assert.match(scrollViewportRule, /-ms-overflow-style:\s*none/);
  assert.match(webkitScrollbarRule, /display:\s*none/);
});

test("shared horizontal scroll arrows are larger controls with a visible shadow", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const arrowRule = stylesheet.match(/\.lux-horizontal-scroll-arrow\s*\{([^}]*)\}/)?.[1] ?? "";
  const hoverRule = stylesheet.match(/\.lux-horizontal-scroll-arrow:hover\s*\{([^}]*)\}/)?.[1] ?? "";
  const arrowIconRule = stylesheet.match(/\.lux-horizontal-scroll-arrow\s+svg\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(arrowRule, /width:\s*48px/);
  assert.match(arrowRule, /height:\s*48px/);
  assert.match(arrowRule, /background:\s*rgba\(12,14,18,\.82\)/);
  assert.match(arrowRule, /box-shadow:\s*0 8px 24px rgba\(0,0,0,\.42\)/);
  assert.match(arrowRule, /backdrop-filter:\s*blur\(5px\)/);
  assert.match(arrowIconRule, /width:\s*24px/);
  assert.match(arrowIconRule, /height:\s*24px/);
  assert.match(hoverRule, /background:\s*rgba\(12,14,18,\.94\)/);
});

test("media cast has no separator line", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const castRule = stylesheet.match(/\.lux-media-cast\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.doesNotMatch(castRule, /border-top/);
});

test("series seasons follow the cast without a separator line", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const seriesChildrenRule = stylesheet.match(/\.lux-series-children(?:,\s*\.lux-season-episodes,\s*\.lux-episode-rail)?\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.doesNotMatch(seriesChildrenRule, /border-top/);
});

test("season poster placeholder styles do not stretch rating or episode badges", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const source = readFileSync(new URL("../src/features/detail/MediaDetailPage.tsx", import.meta.url), "utf8");

  assert.match(stylesheet, /\.lux-season-card-art\s*>\s*\.lux-season-card-placeholder\s*\{/);
  assert.doesNotMatch(stylesheet, /\.lux-season-card-art\s*>\s*span\s*\{/);
  assert.match(source, /className="lux-season-card-placeholder"/);
});

test("detail lower sections use the full content width", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const castRule = stylesheet.match(/\.lux-media-cast\s*\{([^}]*)\}/)?.[1] ?? "";
  const infoRule = stylesheet.match(/\.lux-media-info\s*\{([^}]*)\}/)?.[1] ?? "";
  const infoExtraRule = stylesheet.match(/\.lux-media-info-extra\s*\{([^}]*)\}/)?.[1] ?? "";
  const sourceSelectorRule = stylesheet.match(/\.lux-source-selector\s*\{([^}]*)\}/)?.[1] ?? "";
  const seriesChildrenRule = stylesheet.match(/\.lux-series-children\s*\{([^}]*)\}/)?.[1] ?? "";
  const hierarchyRule = stylesheet.match(/\.lux-season-episodes, \.lux-episode-rail\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(castRule, /max-width:\s*none/);
  assert.match(infoRule, /max-width:\s*none/);
  assert.match(infoExtraRule, /max-width:\s*none/);
  assert.match(sourceSelectorRule, /max-width:\s*none/);
  assert.match(seriesChildrenRule, /max-width:\s*none/);
  assert.match(hierarchyRule, /max-width:\s*none/);
});

test("mobile detail pages use a full-bleed portrait poster and logo-first identity", () => {
  const source = readFileSync(new URL("../src/features/detail/MediaDetailPage.tsx", import.meta.url), "utf8");
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const posterRule = mobileStyles.match(/\.lux-detail-poster\s*\{([^}]*)\}/)?.[1] ?? "";
  const posterColumnRule = mobileStyles.match(/\.lux-detail-poster-column\s*\{([^}]*)\}/)?.[1] ?? "";
  const contentRule = mobileStyles.match(/\.lux-detail-content\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(source, /className="lux-detail-mobile-header"/);
  assert.match(source, /className="lux-detail-title-row" data-has-logo=\{logoUrl \? "true" : undefined\}/);
  assert.match(contentRule, /width:\s*100%/);
  assert.match(posterColumnRule, /width:\s*100%/);
  assert.match(posterRule, /border:\s*0/);
  assert.match(posterRule, /border-radius:\s*0/);
  assert.match(posterRule, /box-shadow:\s*none/);
  assert.match(mobileStyles, /\.lux-detail-backdrop\s*\{[^}]*display:\s*none/);
  assert.match(mobileStyles, /\.lux-detail-title-row\[data-has-logo="true"\]\s+h1\s*\{[^}]*display:\s*none/);
});

test("mobile episode details switch their landscape hero to a portrait poster when available", () => {
  const source = readFileSync(new URL("../src/features/detail/MediaDetailPage.tsx", import.meta.url), "utf8");
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));

  assert.match(source, /className="lux-detail-poster-landscape"/);
  assert.match(source, /className="lux-detail-poster-portrait"/);
  assert.match(source, /isEpisode && poster \? " has-portrait" : ""/);
  assert.match(mobileStyles, /\.lux-detail-poster\.is-landscape\s*\{[^}]*aspect-ratio:\s*2\s*\/\s*3/);
  assert.match(mobileStyles, /\.lux-detail-poster\.is-landscape\.has-portrait \.lux-detail-poster-landscape\s*\{[^}]*display:\s*none/);
  assert.match(mobileStyles, /\.lux-detail-poster\.is-landscape\.has-portrait \.lux-detail-poster-portrait\s*\{[^}]*display:\s*block/);
});

test("mobile detail sections put hierarchy before cast and metadata", () => {
  const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));

  assert.match(mobileStyles, /\.lux-detail-sections\s*\{[^}]*display:\s*flex/);
  assert.match(mobileStyles, /\.lux-detail-hierarchy\s*\{[^}]*order:\s*1/);
  assert.match(mobileStyles, /\.lux-detail-cast\s*\{[^}]*order:\s*2/);
  assert.match(mobileStyles, /\.lux-detail-nfo\s*\{[^}]*order:\s*3/);
  assert.match(mobileStyles, /\.lux-detail-copy\s*>\s*\.lux-hero-actions\s*>\s*\.lux-media-actions\s*\{[^}]*display:\s*none/);
  assert.match(stylesheet, /@media \(max-width: 560px\) \{\s*html\[data-lux-theme="light"\] \.lux-detail-poster\s*\{[^}]*border:\s*0[^}]*box-shadow:\s*none/);
});
