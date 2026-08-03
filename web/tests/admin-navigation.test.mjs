import test from "node:test";
import assert from "node:assert/strict";
import {
  ADMIN_NAV_ITEMS,
  adminRoute,
  adminSectionForRoute,
  isAdminRoute,
  renderAdminNavigation,
} from "../src/admin-navigation.mjs";

test("management navigation exposes the six documented sections", () => {
  assert.deepEqual(
    ADMIN_NAV_ITEMS.map((item) => item.id),
    ["dashboard", "libraries", "users", "jobs", "metadata", "settings"],
  );
  assert.deepEqual(
    ADMIN_NAV_ITEMS.map((item) => item.label),
    ["仪表盘", "媒体库与计划", "用户与权限", "任务与日志", "元数据与图片", "服务端设置"],
  );
});

test("management sections map to stable routes", () => {
  assert.equal(adminRoute("dashboard"), "admin");
  assert.equal(adminRoute("libraries"), "admin-libraries");
  assert.equal(adminRoute("metadata"), "admin-metadata");
  assert.equal(adminSectionForRoute("admin-settings"), "settings");
  assert.equal(adminSectionForRoute("admin"), "dashboard");
  assert.equal(adminSectionForRoute("home"), "dashboard");
});

test("only management routes keep the management navigation expanded", () => {
  assert.equal(isAdminRoute("admin"), true);
  assert.equal(isAdminRoute("admin-jobs"), true);
  assert.equal(isAdminRoute("home"), false);
  assert.equal(isAdminRoute("account"), false);
});

test("expanded management navigation renders every child and marks the active child", () => {
  const html = renderAdminNavigation({ expanded: true, route: "admin-settings" });

  assert.match(html, /data-action="toggle-admin-nav"[^>]+aria-expanded="true"/);
  for (const item of ADMIN_NAV_ITEMS) {
    assert.match(html, new RegExp(`data-route="${adminRoute(item.id)}"`));
  }
  assert.match(html, /data-route="admin-settings" aria-current="page"/);
  assert.match(html, /aria-label="管理设置"/);
});

test("collapsed management navigation hides its child settings", () => {
  const html = renderAdminNavigation({ expanded: false, route: "home" });

  assert.match(html, /aria-expanded="false"/);
  assert.doesNotMatch(html, /id="admin-navigation"/);
  assert.doesNotMatch(html, /data-route="admin-settings"/);
});
