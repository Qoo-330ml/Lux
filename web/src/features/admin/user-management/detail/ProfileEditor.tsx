import { useMutation, useQueryClient } from "@tanstack/react-query";
import { UserRound } from "lucide-react";
import { FormEvent, useEffect, useState } from "react";
import { api } from "../../../../lib/api/client";
import { queryKeys } from "../../../../lib/api/query-keys";
import type { AdminUser } from "../../../../lib/api/types";
import { LuxButton, LuxTextField, luxButtonClass } from "../../../../components/ui";
import { DetailDisclosure } from "../../users/DetailDisclosure";

export function AdminUserProfileEditor({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const [displayName, setDisplayName] = useState(user.displayName ?? "");
  const [email, setEmail] = useState(user.email ?? "");
  const [notice, setNotice] = useState("");
  const [avatarNotice, setAvatarNotice] = useState("");
  const [avatarVersion, setAvatarVersion] = useState(() => String(Date.now()));
  const [avatarFailed, setAvatarFailed] = useState(false);
  const update = useMutation({
    mutationFn: () => api.updateAdminUserProfile(user.id, { displayName, email }),
    onSuccess: ({ user: updated }) => {
      setDisplayName(updated.displayName ?? "");
      setEmail(updated.email ?? "");
      setNotice("账户资料已保存。");
      queryClient.setQueryData(["admin", "user", user.id], { user: updated });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminUsers });
    },
    onError: (error) => setNotice(error.message || "资料保存失败，请重试。"),
  });
  const updateAvatar = useMutation({
    mutationFn: (file: File) => api.uploadAdminUserAvatar(user.id, file),
    onSuccess: () => {
      setAvatarVersion(String(Date.now()));
      setAvatarFailed(false);
      setAvatarNotice("头像已更新。");
    },
    onError: (error) => setAvatarNotice(error.message || "头像更新失败，请重试。"),
  });
  useEffect(() => {
    setDisplayName(user.displayName ?? "");
    setEmail(user.email ?? "");
    setNotice("");
  }, [user.displayName, user.email, user.id]);

  const emailStatus = user.email
    ? user.emailVerifiedAt ? "已验证" : "未验证"
    : "未填写";
  const hasChanges = displayName !== (user.displayName ?? "") || email !== (user.email ?? "");
  const readOnly = user.deletedAt != null;
  const statusLabel = user.deletedAt != null ? "回收站" : user.isDisabled ? "已禁用" : "正常";

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setNotice("");
    update.mutate();
  }

  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-profile-title">
      <div className="lux-admin-panel-heading">
        <div><h2 id="lux-admin-user-profile-title">账户资料</h2></div>
        <span className={readOnly || user.isDisabled ? "lux-user-badge is-warn" : "lux-user-badge is-ok"}>{statusLabel}</span>
      </div>
      <div className="lux-admin-user-profile-summary">
        <span className="lux-admin-user-avatar is-profile">
          {avatarFailed ? <UserRound size={27} /> : <img src={api.adminUserAvatarUrl(user.id, avatarVersion)} alt="" onError={() => setAvatarFailed(true)} />}
        </span>
        <dl className="lux-admin-user-profile-facts">
          <div><dt>显示名称</dt><dd>{user.displayName?.trim() || "未设置"}</dd></div>
          <div><dt>邮箱</dt><dd>{user.email || "未填写"}<small>{emailStatus}</small></dd></div>
          <div><dt>用户名</dt><dd>@{user.usernameNormalized}</dd></div>
        </dl>
      </div>
      {readOnly ? (
        <p className="lux-admin-muted">回收站中的账号资料为只读。</p>
      ) : (
        <DetailDisclosure
          id={`user-profile-${user.id}`}
          openLabel="编辑资料"
          closeLabel="收起编辑"
          hint="需要修改头像、显示名称或邮箱时再展开。"
        >
          <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit}>
            <div className="lux-admin-user-avatar-actions">
              <strong>头像</strong>
              <label className={luxButtonClass({ tone: "secondary", className: "lux-admin-user-avatar-upload" })}>
                <span>{updateAvatar.isPending ? "上传中…" : "更换头像"}</span>
                <input
                  type="file"
                  accept="image/jpeg,image/png,image/webp"
                  aria-label="更换用户头像"
                  disabled={updateAvatar.isPending}
                  onChange={(event) => {
                    const file = event.currentTarget.files?.[0];
                    event.currentTarget.value = "";
                    if (file) {
                      setAvatarNotice("");
                      updateAvatar.mutate(file);
                    }
                  }}
                />
              </label>
              {avatarNotice ? <span role={updateAvatar.isError ? "alert" : "status"}>{avatarNotice}</span> : null}
            </div>
            <label>
              显示名称
              <LuxTextField value={displayName} onChange={(event) => setDisplayName(event.target.value)} maxLength={128} />
            </label>
            <label>
              邮箱
              <LuxTextField type="email" value={email} onChange={(event) => setEmail(event.target.value)} autoComplete="email" />
            </label>
            <p className="lux-admin-muted">{`邮箱状态：${emailStatus}。管理员修改邮箱会清除验证状态；用户可在账户设置保存当前邮箱以发送验证邮件。`}</p>
            <div className="lux-admin-user-profile-actions">
              <LuxButton tone="primary" type="submit" disabled={!hasChanges || update.isPending}>
                {update.isPending ? "保存中…" : "保存资料"}
              </LuxButton>
              {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
            </div>
          </form>
        </DetailDisclosure>
      )}
    </section>
  );
}
