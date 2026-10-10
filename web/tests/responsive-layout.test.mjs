import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const stylesheet = readFileSync(new URL("../src/react.css", import.meta.url), "utf8");
const identifierStyles = readFileSync(new URL("../src/features/media/MediaIdentifier.css", import.meta.url), "utf8");
const luxSelectSource = readFileSync(new URL("../src/components/LuxSelect.tsx", import.meta.url), "utf8");
const pluginStyles = readFileSync(new URL("../src/features/admin/plugin-library.css", import.meta.url), "utf8");
const notificationStyles = readFileSync(new URL("../src/features/admin/notifications.css", import.meta.url), "utf8");
const indexHtml = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const responsiveWidth = /width:\s*92%/;
const fixedPixelWidth = /width:\s*(?:\d+px|min\(\s*\d+px)/;

function rule(selector) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return stylesheet.match(new RegExp(`${escapedSelector}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
}

test("primary page surfaces use viewport-relative widths on large displays", () => {
  for (const selector of [
    ".lux-home-content",
    ".lux-page",
    ".lux-page-narrow",
    ".lux-detail-content",
    ".lux-admin-layout",
  ]) {
    assert.match(rule(selector), responsiveWidth, selector);
  }
});

test("player overlays use viewport-relative safe-area insets", () => {
  const safeAreaInset = /(?:left|right):\s*calc\(4%\s*\+\s*env\(safe-area-inset-(?:left|right)\)\)/;

  assert.match(rule(".lux-player-topbar"), safeAreaInset);
  assert.match(rule(".lux-player-frame"), /left:\s*env\(safe-area-inset-left\)/);
  assert.match(rule(".lux-player-frame"), /right:\s*env\(safe-area-inset-right\)/);
});

test("portrait phone players move AirPlay and picture-in-picture to the top bar", () => {
  assert.match(stylesheet, /\.lux-player-topbar-actions\s*\{[^}]*display:\s*none/);
  assert.match(stylesheet, /@media\s*\(max-width:\s*720px\)\s+and\s+\(orientation:\s*portrait\)[\s\S]*?\.lux-player-topbar-actions\s*\{[^}]*display:\s*flex/);
  assert.match(stylesheet, /@media\s*\(max-width:\s*720px\)\s+and\s+\(orientation:\s*portrait\)[\s\S]*?\.lux-player-controls \.lux-player-mobile-top-control\s*\{[^}]*display:\s*none/);
});

test("center playback action uses a smaller visual footprint", () => {
  assert.match(stylesheet, /\.lux-player-center-play, \.lux-player-center-splash\s*\{[^}]*width:\s*88px;[^}]*height:\s*88px/);
  assert.match(stylesheet, /@media\s*\(max-width:\s*560px\)[\s\S]*?\.lux-player-center-play, \.lux-player-center-splash\s*\{[^}]*width:\s*68px;[^}]*height:\s*68px/);
});

test("volume slider keeps its full track available for both endpoints", () => {
  const volumeRule = rule(".lux-player-volume-slider");

  assert.match(volumeRule, /padding:\s*0/);
  assert.match(volumeRule, /min-height:\s*0/);
  assert.match(volumeRule, /border:\s*0/);
});

test("volume slider keeps a visible drag thumb across its full range", () => {
  const volumeRule = rule(".lux-player-volume-slider");
  const webkitThumbRule = rule(".lux-player-volume-slider::-webkit-slider-thumb");
  const mozThumbRule = rule(".lux-player-volume-slider::-moz-range-thumb");

  assert.match(volumeRule, /appearance:\s*none/);
  assert.match(volumeRule, /-webkit-appearance:\s*none/);
  assert.match(volumeRule, /height:\s*18px/);
  assert.match(webkitThumbRule, /width:\s*12px/);
  assert.match(webkitThumbRule, /height:\s*12px/);
  assert.match(webkitThumbRule, /border-radius:\s*50%/);
  assert.match(mozThumbRule, /border-radius:\s*50%/);
  assert.match(stylesheet, /\.lux-player-volume-slider-wrap:focus-within\s*\{[^}]*overflow:\s*visible/);
});

test("large-display page surfaces do not keep fixed pixel width caps", () => {
  for (const selector of [
    ".lux-home-content",
    ".lux-page",
    ".lux-page-narrow",
    ".lux-detail-content",
    ".lux-player-topbar",
    ".lux-player-frame",
    ".lux-admin-layout",
  ]) {
    assert.doesNotMatch(rule(selector), fixedPixelWidth, selector);
  }
});

test("the global header stays fixed while page content scrolls", () => {
  assert.match(rule(".lux-header"), /position:\s*fixed/);
  assert.match(rule(".lux-header"), /(?:top:\s*0|inset:\s*0\s+0\s+auto)/);
});

test("the fixed header softens scrolling content behind its gradient", () => {
  const veil = rule(".lux-header::before");

  assert.match(rule(".lux-header"), /background:\s*transparent/);
  assert.match(veil, /background:\s*linear-gradient\(/);
  assert.match(veil, /backdrop-filter:\s*blur\(18px\)/);
  assert.match(veil, /-webkit-backdrop-filter:\s*blur\(18px\)/);
});

test("the header blur fades below the toolbar instead of ending on a hard edge", () => {
  const fade = rule(".lux-header::before");

  assert.match(fade, /inset:\s*0\s+0\s+auto/);
  assert.match(fade, /height:\s*calc\(100%\s*\+\s*clamp\(40px,\s*4vw,\s*64px\)\)/);
  assert.match(fade, /backdrop-filter:\s*blur\(18px\)/);
  assert.match(fade, /mask-image:\s*linear-gradient\(/);
  assert.match(fade, /pointer-events:\s*none/);
});

test("mobile navigation stays attached below the fixed header", () => {
  const fixedMobileNavRule = stylesheet.match(/\.lux-mobile-nav\s*\{\s*position:\s*fixed[^}]*\}/)?.[0] ?? "";

  assert.match(fixedMobileNavRule, /position:\s*fixed/);
  assert.match(fixedMobileNavRule, /top:\s*var\(--lux-header-height\)/);
});

test("mobile task activity popover stays inside the viewport and wraps its content", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const popoverRule = mobileStyles.match(/\.lux-scan-activity-popover\s*\{([^}]*)\}/)?.[1] ?? "";
  const headingRule = mobileStyles.match(/\.lux-scan-activity-row-heading\s*\{([^}]*)\}/)?.[1] ?? "";
  const actionRule = mobileStyles.match(/\.lux-scan-activity-actions\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(popoverRule, /position:\s*fixed/);
  assert.match(popoverRule, /left:\s*12px/);
  assert.match(popoverRule, /right:\s*12px/);
  assert.match(popoverRule, /width:\s*auto/);
  assert.match(popoverRule, /overflow-y:\s*auto/);
  assert.match(headingRule, /flex-direction:\s*column/);
  assert.match(actionRule, /flex-wrap:\s*wrap/);
});

test("mobile person details use the same horizontal page gutter", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 700px)"));
  const personRule = mobileStyles.match(/\.lux-person-detail-page\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(personRule, /width:\s*92%/);
  assert.match(personRule, /box-sizing:\s*border-box/);
});

test("touch media cards expose their action controls", () => {
  const touchStyles = stylesheet.match(/@media \(hover: none\), \(pointer: coarse\)\s*\{([\s\S]*?)\n\}/)?.[1] ?? "";

  assert.match(touchStyles, /\.lux-media-art-shell > \.lux-media-actions\s*\{[^}]*opacity:\s*1[^}]*pointer-events:\s*auto/s);
  assert.match(touchStyles, /\.lux-library-card-menu-trigger\s*\{[^}]*opacity:\s*1/s);
});

test("touch text controls keep a 16px floor above page-specific font sizes", () => {
  const touchStyles = stylesheet.match(/@media \(pointer: coarse\)\s*\{([\s\S]*?)\n\}/)?.[1] ?? "";

  assert.match(touchStyles, /textarea, select\)\s*\{[^}]*font-size:\s*max\(16px,\s*var\(--lux-input-font-size,\s*1em\)\)\s*!important/);
  for (const type of ["checkbox", "radio", "file", "hidden", "range", "color", "button", "submit", "reset", "image"]) {
    assert.ok(touchStyles.includes(`:not([type="${type}"])`), `${type} is not a text control`);
  }
});

test("touch text sizing preserves large editable titles and manual page zoom", () => {
  assert.match(rule(".lux-person-title-input"), /--lux-input-font-size:\s*clamp\(1\.6rem,\s*4vw,\s*2\.8rem\)/);
  assert.match(rule(".lux-person-title-input"), /font-size:\s*var\(--lux-input-font-size\)/);
  assert.doesNotMatch(indexHtml, /user-scalable\s*=\s*(?:no|0)|maximum-scale\s*=/i);
});

test("mobile library batch actions wrap instead of overflowing", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const selectionRule = mobileStyles.match(/\.lux-library-selection-toolbar\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(selectionRule, /flex-wrap:\s*wrap/);
});

test("mobile image editing controls stay usable in a narrow dialog", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const tabsRule = mobileStyles.match(/\.lux-image-type-tabs\s*\{([^}]*)\}/)?.[1] ?? "";
  const toolbarRule = mobileStyles.match(/\.lux-image-editor-toolbar\s*\{([^}]*)\}/)?.[1] ?? "";
  const buttonRule = mobileStyles.match(/\.lux-image-editor-toolbar > \.lux-button\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(tabsRule, /overflow-x:\s*auto/);
  assert.match(tabsRule, /display:\s*flex/);
  assert.match(toolbarRule, /grid-template-columns:\s*repeat\(2/);
  assert.match(buttonRule, /grid-column:\s*1\s*\/\s*-1/);
});

test("mobile player controls use the remaining width", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const controlsRule = mobileStyles.match(/\.lux-player-controls-right\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(controlsRule, /flex:\s*1 1 auto/);
  assert.match(controlsRule, /width:\s*auto/);
  assert.match(controlsRule, /max-width:\s*none/);
});

test("mobile action surfaces scroll within the dynamic viewport", () => {
  const actionMenuRule = rule(".lux-media-action-menu");
  const settingsRule = rule(".lux-player-settings-popover");

  assert.match(actionMenuRule, /max-height:\s*[^;]*dvh/);
  assert.match(actionMenuRule, /overflow-y:\s*auto/);
  assert.match(settingsRule, /max-height:\s*[^;]*dvh/);
  assert.match(settingsRule, /overflow-y:\s*auto/);
});

test("mobile metadata identifier results keep a usable landscape height", () => {
  const mobileRules = identifierStyles.slice(identifierStyles.indexOf("@media (max-width: 560px)"));
  assert.match(mobileRules, /\.lux-identifier-results\s*\{[^}]*max-height:\s*[^;]*100dvh[^;]*-\s*180px/s);
});

test("mobile header reserves the safe-area inset", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const headerRule = mobileStyles.match(/\.lux-header\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(headerRule, /padding-top:\s*max\([^)]*safe-area-inset-top/);
  assert.match(headerRule, /padding-(?:left|right):\s*max\([^)]*safe-area-inset-(?:left|right)/);
});

test("mobile admin operations tabs stay usable without clipping", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const tabsRule = mobileStyles.match(/\.lux-operations-tabs\s*\{([^}]*)\}/)?.[1] ?? "";
  const tabRule = mobileStyles.match(/\.lux-operations-tab\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(tabsRule, /overflow-x:\s*auto/);
  assert.match(tabRule, /flex:\s*0 0 auto/);
  assert.match(tabRule, /white-space:\s*nowrap/);
});

test("admin mobile dialogs use dynamic viewport and safe-area insets", () => {
  for (const source of [stylesheet, pluginStyles, notificationStyles]) {
    assert.match(source, /env\(safe-area-inset-(?:top|right|bottom|left)\)/);
    assert.match(source, /100dvh/);
  }
});

test("admin mobile controls use the touch target token", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 900px)"));
  assert.match(mobileStyles, /\.lux-admin-nav-link\s*\{[^}]*min-height:\s*var\(--lux-button-height-touch\)/);
  assert.match(mobileStyles, /\.lux-admin-filter-select \.lux-select-trigger\s*\{[^}]*min-height:\s*var\(--lux-button-height-touch\)/);
  assert.match(mobileStyles, /\.lux-admin-user-actions \.lux-icon-button-small[^}]*min-height:\s*var\(--lux-button-height-touch\)/);
  assert.match(mobileStyles, /\.lux-directory-tree-toggle\s*\{[^}]*height:\s*var\(--lux-button-height-touch\)/);
  assert.match(pluginStyles, /\.lux-admin-plugin-tabs button\s*\{[^}]*min-height:\s*var\(--lux-button-height-touch\)/);
  assert.match(notificationStyles, /\.lux-notification-destination-actions \.lux-button\s*\{[^}]*min-height:\s*var\(--lux-button-height-touch\)/);
});

test("admin mobile dashboard metadata remains readable", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  assert.match(mobileStyles, /\.lux-now-playing-account\s*\{[^}]*font-size:\s*\.68rem/);
  assert.match(mobileStyles, /\.lux-now-playing-progress-label\s*\{[^}]*font-size:\s*\.66rem/);
  assert.match(mobileStyles, /\.lux-now-playing-fact small[^}]*font-size:\s*\.64rem/);
});

test("admin permission toggles collapse to one column on narrow phones", () => {
  const narrowStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 460px)"));
  assert.match(narrowStyles, /\.lux-admin-permission-grid\s*\{[^}]*grid-template-columns:\s*1fr/);
  assert.match(narrowStyles, /\.lux-admin-permission-toggle\s*\{[^}]*min-height:\s*var\(--lux-button-height-touch\)/);
});

test("LuxSelect tracks the visual viewport while its menu is open", () => {
  assert.match(luxSelectSource, /window\.visualViewport/);
  assert.match(luxSelectSource, /visualViewport\?\.addEventListener\("resize"/);
  assert.match(luxSelectSource, /visualViewport\?\.addEventListener\("scroll"/);
  assert.match(luxSelectSource, /visualViewport\?\.height/);
});

test("mobile detail pages keep their vertical content scrollable", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const detailRule = mobileStyles.match(/\.lux-detail-page\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(detailRule, /overflow-x:\s*clip/);
  assert.match(detailRule, /overflow-y:\s*auto/);
});

test("mobile viewport metadata and auth shell respect browser chrome", () => {
  assert.match(indexHtml, /viewport-fit=cover/);
  assert.match(indexHtml, /interactive-widget=resizes-content/);
  const authStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 1024px)"));
  assert.match(authStyles, /\.lux-auth-screen\s*\{[^}]*min-height:\s*100dvh/);
  assert.match(authStyles, /\.lux-auth-screen\s*\{[^}]*overflow-y:\s*auto/);
  assert.match(authStyles, /safe-area-inset-top/);
});

test("detail and media editor overlays reserve safe areas and dynamic height", () => {
  const detailBackdrop = rule(".lux-detail-overview-dialog-backdrop");
  const detailDialog = rule(".lux-detail-overview-dialog");
  const detailBody = rule(".lux-detail-overview-dialog-body");
  const editorBackdrop = rule(".lux-media-editor-backdrop");
  const editor = rule(".lux-media-editor");

  assert.match(detailBackdrop, /safe-area-inset-(?:top|right|bottom|left)/);
  assert.match(detailDialog, /100dvh/);
  assert.match(detailBody, /100dvh/);
  assert.match(editorBackdrop, /safe-area-inset-(?:top|right|bottom|left)/);
  assert.match(editor, /100dvh/);
});

test("narrow media editor footers wrap their actions", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 560px)"));
  const footerRule = mobileStyles.match(/\.lux-media-editor-footer\s*\{([^}]*)\}/)?.[1] ?? "";
  const footerActionsRule = mobileStyles.match(/\.lux-media-editor-footer\s*>\s*div\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(footerRule, /flex-wrap:\s*wrap/);
  assert.match(footerActionsRule, /flex:\s*1 1 100%/);
});

test("touch account controls retain a usable hit area", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 900px)"));

  assert.match(mobileStyles, /\.lux-account-settings-nav a\s*\{[^}]*min-height:\s*var\(--lux-button-height-touch\)/);
  assert.match(mobileStyles, /\.lux-account-library-actions button\s*\{[^}]*min-height:\s*var\(--lux-button-height-touch\)/);
});

test("mobile admin library surface expands with its negative outer gutter", () => {
  const mobileStyles = stylesheet.slice(stylesheet.indexOf("@media (max-width: 720px)"));
  const libraryPageRule = mobileStyles.match(/\.lux-admin-library-page\s*\{([^}]*)\}/)?.[1] ?? "";

  assert.match(libraryPageRule, /width:\s*auto/);
});
