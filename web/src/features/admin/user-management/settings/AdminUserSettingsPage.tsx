import { useSearchParams } from "react-router-dom";
import { AdminUserGlobalAccessRulesPanel } from "../../AdminUserGlobalAccessRulesPanel";
import { UsersWorkspace, type PolicySectionKey } from "../../users/UsersWorkspace";
import { AdminUserDefaultPermissionsSettings } from "./DefaultPermissionsSettings";
import { AdminUserMailSettings } from "./MailSettingsPanel";
import { AdminUserPermissionPresetsSettings } from "./PermissionPresetsSettings";
import { AdminUserRegistrationSettings } from "./RegistrationSettings";
import { AdminUserRetentionSettings } from "./RetentionSettings";
import { TrustedProxyPlaceholder } from "./TrustedProxyPlaceholder";

const policyGroups = [
  { id: "registration", sections: ["registration", "mail"] },
  { id: "defaults", sections: ["defaults", "presets"] },
  { id: "blocklist", sections: ["blocklist", "proxy"] },
  { id: "retention", sections: ["retention"] },
] as const;

type PolicyGroupId = (typeof policyGroups)[number]["id"];

export function AdminUserSettingsPage() {
  const [searchParams] = useSearchParams();
  const requested = searchParams.get("section") ?? "registration";
  const activeSection = (["registration", "mail", "defaults", "presets", "blocklist", "proxy", "retention"].includes(requested) ? requested : "registration") as PolicySectionKey;
  const groupId: PolicyGroupId = policyGroups.find((group) => (group.sections as readonly string[]).includes(requested))?.id ?? "registration";

  return (
    <div className="lux-admin-page lux-users-page">
      <UsersWorkspace activeSection={activeSection}>
        <div className="lux-users-policy-body">
          {groupId === "registration" ? (
            <>
              <AdminUserRegistrationSettings />
              <AdminUserMailSettings />
            </>
          ) : groupId === "defaults" ? (
            <>
              <AdminUserPermissionPresetsSettings />
              <AdminUserDefaultPermissionsSettings />
            </>
          ) : groupId === "blocklist" ? (
            <>
              <AdminUserGlobalAccessRulesPanel />
              <TrustedProxyPlaceholder />
            </>
          ) : (
            <AdminUserRetentionSettings />
          )}
        </div>
      </UsersWorkspace>
    </div>
  );
}
