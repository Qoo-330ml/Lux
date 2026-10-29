import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const read = (path) => fs.readFileSync(new URL(path, import.meta.url), "utf8");
const uiCss = read("../src/components/ui/ui.css");
const reactCss = read("../src/react.css");
const component = read("../src/components/ui/LuxProgress.tsx");

/** 取出某个样式表里所有包含 .lux-progress 的选择器。 */
function selectorsWithProgress(css) {
  const selectors = [];
  for (const match of css.matchAll(/([^{}]+)\{/g)) {
    const head = match[1].trim();
    if (head.startsWith("@") || !head.includes("lux-progress")) continue;
    for (const part of head.split(",")) {
      if (part.includes("lux-progress")) selectors.push(part.trim());
    }
  }
  return selectors;
}

test("UI 进度条使用独立类名，不再复用首页卡片的 .lux-progress", () => {
  assert.match(uiCss, /\.lux-progress-bar\s*\{/, "UI 进度条根节点应有独立类名 .lux-progress-bar");
  assert.doesNotMatch(
    uiCss,
    /(^|\n)\.lux-progress\s*\{/,
    "ui.css 不得再定义裸 .lux-progress：react.css 把它当作绝对定位的 4px 卡片进度条，两者会互相污染",
  );
  assert.match(component, /cx\("lux-progress-bar"/, "LuxProgress 根节点必须输出 .lux-progress-bar");
});

test("首页卡片的 .lux-progress 只在媒体缩略图内生效", () => {
  const selectors = selectorsWithProgress(reactCss);
  assert.ok(selectors.length > 0, "react.css 应保留首页卡片的进度条样式");
  for (const selector of selectors) {
    assert.match(
      selector,
      /^\.lux-media-art\s+\.lux-progress/,
      `首页进度条选择器 ${selector} 必须限定在 .lux-media-art 内，否则会命中管理页的进度组件，渲染成一条横贯卡片的细线`,
    );
  }
});

test("进度条轨道在栅格容器内能撑满，不会塌成 0 宽", () => {
  const bar = uiCss.match(/\.lux-progress-bar\s*\{([^}]*)\}/)?.[1] ?? "";
  const track = uiCss.match(/\.lux-progress-track\s*\{([^}]*)\}/)?.[1] ?? "";
  assert.match(bar, /grid-template-columns:\s*minmax\(0,\s*1fr\)/, "缺少 1fr 列时，grid 的 auto 列会按内容宽度收缩，轨道会塌成 0 宽");
  assert.match(track, /width:\s*100%/, "轨道的百分比填充需要明确的 100% 宽度作为参照");
});
