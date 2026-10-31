import type { AdminUser } from "../../../../../lib/api/types";
import { AdminUserAccessRulesPanel } from "../../../AdminUserAccessRulesPanel";
import { AdminUserDevicePanel } from "./DevicePanel";
import { AdminUserEmbySessionsPanel } from "./EmbySessionsPanel";
import { AdminUserPlaybackSessionsPanel } from "./PlaybackSessionsPanel";
import { AdminUserWebSessionsPanel } from "./WebSessionsPanel";

export function AdminUserSessionsSection({ user }: { user: AdminUser }) {
  return (
    <>
      <AdminUserDevicePanel user={user} />
      <AdminUserAccessRulesPanel userId={user.id} />
      <AdminUserWebSessionsPanel user={user} />
      <AdminUserEmbySessionsPanel user={user} />
      <AdminUserPlaybackSessionsPanel user={user} />
    </>
  );
}
