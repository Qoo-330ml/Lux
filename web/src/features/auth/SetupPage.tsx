import { useMutation, useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { AdminSetupForm } from "./AdminSetupForm";
import { DatabaseSetupPanel } from "./DatabaseSetupPanel";
import { LuxLogo } from "../../components/LuxLogo";

export function SetupPage() {
  const [restartRequested, setRestartRequested] = useState(false);
  const database = useQuery({
    queryKey: queryKeys.setupDatabase,
    queryFn: () => api.setupDatabaseStatus(),
    retry: false,
    refetchInterval: restartRequested ? 1000 : false,
  });
  const restart = useMutation({
    mutationFn: () => api.restartSetupDatabase(),
    onSuccess: () => setRestartRequested(true),
  });

  useEffect(() => {
    if (restartRequested && database.data?.configured && !database.data.restartRequired) {
      setRestartRequested(false);
    }
  }, [database.data, restartRequested]);

  if (database.isPending) {
    return <main className="lux-state-screen" aria-busy="true"><div className="lux-spinner" aria-hidden="true" /><p>正在检查数据库配置</p></main>;
  }
  if (restartRequested) {
    return <main className="lux-state-screen" role="status" aria-busy="true"><div className="lux-spinner" aria-hidden="true" /><h1>正在重启 Lux</h1><p>数据库迁移完成后会自动继续管理员初始化，请不要关闭此页面。</p></main>;
  }
  if (database.error) {
    return <main className="lux-state-screen" role="alert"><h1>无法读取数据库配置</h1><p>{database.error.message}</p></main>;
  }
  if (database.data.restartRequired) {
    return <main className="lux-auth-screen"><section className="lux-auth-card lux-setup-card"><div className="lux-auth-brand"><LuxLogo className="lux-brand-logo" /><strong>Lux</strong></div><h1>请重启 Lux</h1><p>PostgreSQL 配置已经保存。重启 Lux 后，系统会运行迁移并继续管理员初始化。</p><button className="lux-button lux-button-primary" type="button" onClick={() => restart.mutate()} disabled={restart.isPending}>{restart.isPending ? "正在准备重启…" : "重启 Lux"}</button>{restart.error ? <p className="lux-error-copy" role="alert">{restart.error.message}</p> : <p className="lux-success-copy" role="status">重启期间页面会暂时无法访问，服务恢复后将自动继续。</p>}</section></main>;
  }
  if (!database.data.configured) {
    return <main className="lux-auth-screen"><DatabaseSetupPanel onSelected={() => undefined} /></main>;
  }
  return <main className="lux-auth-screen"><AdminSetupForm /></main>;
}
