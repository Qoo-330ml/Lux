import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { api } from "../../../../lib/api/client";
import type { AdminUser } from "../../../../lib/api/types";
import { LuxButton, LuxTextField } from "../../../../components/ui";
import { DetailDisclosure } from "../../users/DetailDisclosure";

export function AdminUserPasswordPanel({ user }: { user: AdminUser }) {
  const queryClient = useQueryClient();
  const [password, setPassword] = useState("");
  const [notice, setNotice] = useState("");
  const update = useMutation({
    mutationFn: () => api.updateAdminUser(user.id, { password }),
    onSuccess: () => {
      setPassword("");
      setNotice("密码已更新。");
      void queryClient.invalidateQueries({ queryKey: ["admin", "user", user.id] });
    },
    onError: (error) => setNotice(`密码更新失败：${error.message}`),
  });
  const readOnly = user.deletedAt != null;
  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel lux-admin-user-password-panel" aria-labelledby="lux-admin-user-password-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-password-title">密码</h2></div></div>
      {readOnly ? (
        <p className="lux-admin-muted">回收站中的账号不可修改密码；恢复账号后可继续重置。</p>
      ) : (
        <DetailDisclosure
          id={`user-password-${user.id}`}
          openLabel="修改密码"
          closeLabel="收起"
          hint="修改后该用户所有已登录设备都需要重新登录。"
        >
          <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={(event) => { event.preventDefault(); if (!password) return; setNotice(""); update.mutate(); }}>
            <label>
              设置新密码
              <LuxTextField type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="new-password" placeholder="留空不修改" />
            </label>
            <div className="lux-admin-user-profile-actions">
              <LuxButton tone="primary" type="submit" disabled={!password || update.isPending}>{update.isPending ? "保存中…" : "保存新密码"}</LuxButton>
              {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
            </div>
          </form>
        </DetailDisclosure>
      )}
    </section>
  );
}
