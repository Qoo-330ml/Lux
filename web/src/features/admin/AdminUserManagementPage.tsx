/**
 * Admin user pages are assembled from the panels under `user-management/`.
 * This module stays the public entry point so route lazy-loading and tests keep
 * importing the same specifiers.
 */
import "./user-management.css";

export { AdminUserDetailPage } from "./user-management/detail/AdminUserDetailPage";
export { AdminUserSettingsPage } from "./user-management/settings/AdminUserSettingsPage";
