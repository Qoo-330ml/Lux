import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const css = fs.readFileSync(new URL("../src/features/admin/users-workspace.css", import.meta.url), "utf8");

/** 按花括号配对取出所有 @media 块，返回 {start, condition, body}。 */
function mediaBlocks(source) {
  const blocks = [];
  const opener = /@media[^{]*\{/g;
  for (let match = opener.exec(source); match; match = opener.exec(source)) {
    let depth = 1;
    let index = match.index + match[0].length;
    const start = match.index;
    while (index < source.length && depth > 0) {
      if (source[index] === "{") depth += 1;
      else if (source[index] === "}") depth -= 1;
      index += 1;
    }
    blocks.push({ start, condition: match[0].slice(0, -1).trim(), body: source.slice(start, index) });
  }
  return blocks;
}

const blocks = mediaBlocks(css);

function indexOfRule(selector, declaration, from = 0) {
  return css.search(new RegExp(`\\.${selector}\\s*\\{\\s*${declaration}`, "gm"));
}

test("手机端用户列表卡片不会被后面声明的隐藏规则覆盖", () => {
  const hidden = indexOfRule("lux-users-cards", "display:\\s*none");
  assert.ok(hidden >= 0, "桌面端应隐藏手机卡片布局");

  const mobileCardBlocks = blocks.filter((block) => /\.lux-users-cards\s*\{\s*display:\s*grid/.test(block.body));
  assert.ok(mobileCardBlocks.length > 0, "窄屏断点内应把用户列表切换成卡片布局");

  for (const block of mobileCardBlocks) {
    assert.ok(
      hidden < block.start,
      `\`.lux-users-cards { display: none }\` 必须写在 ${block.condition} 之前，否则会覆盖窄屏卡片布局，导致列表整块不显示`,
    );
  }
});

test("窄屏隐藏表格的同一个断点里必须同时显示卡片列表", () => {
  const hidingBlocks = blocks.filter((block) => /\.lux-users-table-scroll\s*\{\s*display:\s*none/.test(block.body));
  assert.ok(hidingBlocks.length > 0, "应存在窄屏隐藏表格的断点");
  for (const block of hidingBlocks) {
    assert.match(
      block.body,
      /\.lux-users-cards\s*\{\s*display:\s*grid/,
      `${block.condition} 隐藏了表格，但同一断点没有启用卡片列表，窄屏会整块空白`,
    );
  }
});

test("卡片列表覆盖「角色与访问」信息", () => {
  const page = fs.readFileSync(new URL("../src/features/admin/AdminUsersPage.tsx", import.meta.url), "utf8");
  const cardsBlock = page.slice(page.indexOf('className="lux-users-cards"'));
  assert.match(cardsBlock, /lux-users-tags/, "手机卡片必须渲染角色与访问标签，避免切到卡片后丢失该列信息");
  assert.match(cardsBlock, /view === "deleted"/, "回收站视图也要有卡片列表，否则窄屏看不到已删除账号");
});
