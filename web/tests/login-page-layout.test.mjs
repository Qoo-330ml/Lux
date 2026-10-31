import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { baseRule, mediaStyles, stylesheet as allStyles } from "./style-source.mjs";

const stylesheet = allStyles;

test("login split layout keeps one constant left/right ratio at every desktop width", () => {
  const splitLayoutRule = stylesheet.match(/\.lux-auth-screen\.lux-auth-split-layout\s*\{([^}]*)\}/)?.[1] ?? "";
  const visualShare = Number(splitLayoutRule.match(/--lux-auth-split-visual:\s*([\d.]+)%/)?.[1]);
  const panelShare = Number(splitLayoutRule.match(/--lux-auth-split-panel:\s*([\d.]+)%/)?.[1]);

  assert.ok(Number.isFinite(visualShare), "split layout must declare --lux-auth-split-visual");
  assert.ok(Number.isFinite(panelShare), "split layout must declare --lux-auth-split-panel");
  assert.equal(visualShare + panelShare, 100, "visual and panel shares must add up to 100%");
  assert.ok(panelShare >= 30 && panelShare <= 45, `panel share ${panelShare}% must stay a moderate split`);

  const visualRule = baseRule(".lux-auth-visual");
  const panelRule = baseRule(".lux-auth-panel");

  assert.match(visualRule, /flex:\s*0 0 var\(--lux-auth-split-visual/);
  assert.match(visualRule, /width:\s*var\(--lux-auth-split-visual/);
  assert.match(panelRule, /flex:\s*0 0 var\(--lux-auth-split-panel/);
  assert.match(panelRule, /width:\s*var\(--lux-auth-split-panel/);
  assert.doesNotMatch(panelRule, /width:\s*\d+px/, "panel must not keep a fixed pixel width");
});

test("login split layout collapses to one column on narrow viewports", () => {
  const mobileStyles = mediaStyles("(max-width: 1024px)");

  assert.match(mobileStyles, /\.lux-auth-visual\s*\{\s*display:\s*none/);
  assert.match(mobileStyles, /\.lux-auth-panel\s*\{[^}]*width:\s*100%/);
});

test("login background art fills the left visual column edge to edge", () => {
  const visualRule = baseRule(".lux-auth-visual");

  assert.match(visualRule, /min-height:\s*100dvh/);

  for (const [name, selector] of [
    ["poster wall", ".lux-auth-poster-wall"],
    ["hero image", ".lux-auth-hero-image"],
    ["single image", ".lux-auth-single-image"],
    ["single poster backdrop", ".lux-auth-single-poster-backdrop"],
  ]) {
    const rule = baseRule(selector);
    assert.match(rule, /position:\s*absolute/, `${name} must leave the flex flow`);
    assert.match(rule, /inset:\s*0/, `${name} must pin to the visual box`);
    assert.match(rule, /height:\s*100%/, `${name} must span the full visual height`);
    assert.match(rule, /object-fit:\s*cover/, `${name} must cover the visual box`);
  }

  // 单幅海报保持完整原比例、居中叠在模糊铺底之上，不再拉成 cover 裁切。
  const posterRule = baseRule(".lux-auth-single-poster");
  assert.match(posterRule, /position:\s*absolute/);
  assert.match(posterRule, /translate\(-50%,\s*-50%\)/);
  assert.match(posterRule, /max-width:\s*min\(76%,\s*560px\)/);
  assert.match(posterRule, /max-height:\s*82vh/);
  assert.doesNotMatch(posterRule, /object-fit:\s*cover/);

  // 单幅图改成绝对定位铺满后，署名叠加在视觉区上，不再需要居中的内容包裹层。
  assert.doesNotMatch(stylesheet, /\.lux-auth-single-image-content/);
});

test("login poster waterfall keeps the requested scale, inset, and stagger", () => {
  const waterfallRule = stylesheet.match(/\.lux-auth-poster-waterfall\s*\{([^}]*)\}/)?.[1] ?? "";
  const middleColumnRule = stylesheet.match(/\.lux-auth-poster-waterfall-column:nth-child\(2\)\s*\{([^}]*)\}/)?.[1] ?? "";
  const thirdColumnRule = stylesheet.match(/\.lux-auth-poster-waterfall-column:nth-child\(3\)\s*\{([^}]*)\}/)?.[1] ?? "";
  const fourthColumnRule = stylesheet.match(/\.lux-auth-poster-waterfall-column:nth-child\(4\)\s*\{([^}]*)\}/)?.[1] ?? "";
  const fifthColumnRule = stylesheet.match(/\.lux-auth-poster-waterfall-column:nth-child\(5\)\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(waterfallRule, /left:\s*20%/);
  assert.match(waterfallRule, /right:\s*-4%/);
  assert.match(waterfallRule, /grid-template-columns:\s*repeat\(5,\s*minmax\(0,\s*var\(--lux-auth-poster-column-width,\s*26%\)\)\)/);
  assert.match(waterfallRule, /column-gap:\s*12px/);
  assert.match(waterfallRule, /row-gap:\s*12px/);
  assert.match(waterfallRule, /justify-content:\s*start/);
  assert.match(waterfallRule, /transform:\s*rotate\(9deg\)/);
  assert.match(waterfallRule, /transform-origin:\s*left center/);
  assert.match(stylesheet, /\.lux-auth-poster-waterfall-column\s*\{[^}]*align-items:\s*flex-end[^}]*gap:\s*12px/);
  assert.match(middleColumnRule, /margin-top:\s*-15%/);
  assert.match(thirdColumnRule, /margin-top:\s*-30%/);
  assert.match(fourthColumnRule, /margin-top:\s*-45%/);
  assert.match(fifthColumnRule, /margin-top:\s*-60%/);
  assert.match(stylesheet, /\.lux-auth-poster-waterfall img\s*\{[^}]*width:\s*100%/);
});
