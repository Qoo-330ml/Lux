import type { AdminUser } from "../../../../lib/api/types";
import { AdminUserContentIndexStatus } from "./ContentIndexStatus";
import { AdminUserContentRestrictionEditor } from "./ContentRestrictionEditor";

export function AdminUserContentRestrictionSection({ user }: { user: AdminUser }) {
  return (
    <div className="lux-admin-user-content-section">
      <AdminUserContentRestrictionEditor key={user.id} user={user} />
      <AdminUserContentIndexStatus />
    </div>
  );
}
