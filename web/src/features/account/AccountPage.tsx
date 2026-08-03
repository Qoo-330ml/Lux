import { useMutation, useQueryClient } from "@tanstack/react-query";
import { LogOut, Monitor, UserRound } from "lucide-react";
import { useNavigate } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { LuxUser } from "../../lib/api/types";

export function AccountPage({ user }: { user: LuxUser }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const logout = useMutation({
    mutationFn: () => api.logout(),
    onSuccess: () => {
      queryClient.removeQueries({ queryKey: queryKeys.me });
      navigate("/login", { replace: true });
    },
  });

  return (
    <section className="lux-page lux-page-narrow">
      <div className="lux-page-heading"><span className="lux-eyebrow">YOUR LUX PROFILE</span><h1>账户</h1></div>
      <div className="lux-account-card">
        <div className="lux-account-avatar"><UserRound size={28} /></div>
        <div><h2>{user.displayName || user.usernameNormalized}</h2><p>{user.usernameNormalized}</p></div>
      </div>
      <div className="lux-account-list">
        <div><Monitor size={18} /><span>当前设备</span><small>Web 浏览器</small></div>
        <div><UserRound size={18} /><span>权限</span><small>{user.canManageServer ? "服务器管理员" : "普通用户"}</small></div>
      </div>
      <button className="lux-button lux-button-secondary" type="button" onClick={() => logout.mutate()} disabled={logout.isPending}><LogOut size={17} /> {logout.isPending ? "正在退出…" : "退出登录"}</button>
      {logout.error ? <p className="lux-error-copy">{logout.error.message}</p> : null}
    </section>
  );
}
