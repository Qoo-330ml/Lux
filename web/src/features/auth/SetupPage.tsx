import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { AdminSetupForm } from "./AdminSetupForm";
import { DatabaseSetupPanel } from "./DatabaseSetupPanel";
import { LuxLogo } from "../../components/LuxLogo";

export function SetupPage() {
  const database = useQuery({
    queryKey: queryKeys.setupDatabase,
    queryFn: () => api.setupDatabaseStatus(),
    retry: false,
  });

  if (database.isPending) {
    return <main className="lux-state-screen" aria-busy="true"><div className="lux-spinner" aria-hidden="true" /><p>正在检查数据库配置</p></main>;
  }
  if (database.error) {
    return <main className="lux-state-screen" role="alert"><h1>无法读取数据库配置</h1><p>{database.error.message}</p></main>;
  }
  if (database.data.restartRequired) {
    return <main className="lux-auth-screen"><section className="lux-auth-card lux-setup-card"><div className="lux-auth-brand"><LuxLogo className="lux-brand-logo" /><strong>Lux</strong></div><h1>请重启 Lux</h1><p>PostgreSQL 配置已经保存。重启 Lux 后，系统会运行迁移并继续管理员初始化。</p></section></main>;
  }
  if (!database.data.configured) {
    return <main className="lux-auth-screen"><DatabaseSetupPanel onSelected={() => undefined} /></main>;
  }
  return <main className="lux-auth-screen"><AdminSetupForm /></main>;
}
