import type { AdminUser } from "../../../../lib/api/types";
import { AdminUserLibraryAccessEditor } from "./LibraryAccessEditor";
import { AdminUserPermissionPresetEditor } from "./PermissionPresetEditor";

export function AdminUserPermissionsSection({ user }: { user: AdminUser }) {
  if (user.deletedAt != null) {
    return (
      <section className="lux-admin-panel lux-admin-user-permissions" aria-labelledby="lux-admin-user-permissions-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-permissions-title">权限</h2></div></div>
        <p className="lux-admin-muted">回收站中的账号权限为只读，恢复后可修改。</p>
      </section>
    );
  }
  return (
    <>
      <AdminUserPermissionPresetEditor user={user} />
      <AdminUserLibraryAccessEditor user={user} />
    </>
  );
}
