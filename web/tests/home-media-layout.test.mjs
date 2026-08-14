import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
const mediaSource = readFileSync(new URL("../src/features/home/media.tsx", import.meta.url), "utf8");

test("home media actions are hidden until the poster is hovered and sit in its upper-right corner", () => {
  const actionsRule = stylesheet.match(/\.lux-media-art-shell\s*>\s*\.lux-media-actions\s*\{([^}]*)\}/)?.[1] ?? "";
  const visibleRule = stylesheet.match(/\.lux-media-art-shell:hover\s*>\s*\.lux-media-actions,\s*\.lux-media-art-shell:focus-within\s*>\s*\.lux-media-actions\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(mediaSource, /className="lux-media-art-shell"/);
  assert.match(mediaSource, /<Rating value=\{item\.rating\} source=\{item\.ratingSource\} compact=\{compactRating\} placement="card" \/>/);
  assert.match(mediaSource, /<Rating value=\{item\.rating\} source=\{item\.ratingSource\} compact placement="card" \/>/);
  assert.match(actionsRule, /top:\s*9px/);
  assert.match(actionsRule, /bottom:\s*auto/);
  assert.match(actionsRule, /opacity:\s*0/);
  assert.match(actionsRule, /pointer-events:\s*none/);
  assert.match(visibleRule, /opacity:\s*1/);
  assert.match(visibleRule, /pointer-events:\s*auto/);
});

test("detail ratings stay upper-right while home and library card ratings move upper-left", () => {
  const ratingRule = stylesheet.match(/\.lux-rating\s*\{([^}]*)\}/)?.[1] ?? "";
  const cardRatingRule = stylesheet.match(/\.lux-card-rating\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(ratingRule, /position:\s*absolute/);
  assert.match(ratingRule, /top:\s*9px/);
  assert.match(ratingRule, /right:\s*9px/);
  assert.match(cardRatingRule, /left:\s*9px/);
  assert.match(cardRatingRule, /right:\s*auto/);
  const compactRatingRule = stylesheet.match(/\.lux-rating\.is-compact\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(compactRatingRule, /background:\s*#01b4e4/);
  assert.match(mediaSource, /ratingSource/);
});

test("series cards render the episode count in the poster upper-right corner", () => {
  assert.equal((mediaSource.match(/<EpisodeCount item=\{item\} \/>/g) ?? []).length, 2);

  const episodeCountRule = stylesheet.match(/\.lux-media-episode-count\s*\{([^}]*)\}/)?.[1] ?? "";
  assert.match(episodeCountRule, /position:\s*absolute/);
  assert.match(episodeCountRule, /top:\s*9px/);
  assert.match(episodeCountRule, /right:\s*9px/);
});

test("homepage library cards wrap into an adaptive grid instead of one horizontal row", () => {
  const libraryRailRule = stylesheet.match(/\.lux-library-rail\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(libraryRailRule, /display:\s*grid/);
  assert.match(libraryRailRule, /grid-template-columns:\s*repeat\(auto-fill,\s*minmax\(min\(210px,\s*100%\),\s*1fr\)\)/);
  assert.match(libraryRailRule, /width:\s*100%/);
  assert.match(libraryRailRule, /min-width:\s*0/);
});
