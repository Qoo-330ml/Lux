import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { AdminSetupForm } from "./AdminSetupForm";
import { DatabaseSetupPanel } from "./DatabaseSetupPanel";
import { LuxLogo } from "../../components/LuxLogo";
import { LuxButton } from "../../components/ui";

// Cadence of the readiness probes while the process restarts, and the fallback
// deadline for the case where the port never actually closes (restart skipped,
// or a reverse proxy that keeps answering). See docs/SETUP-RESTART-DESIGN.md §5.3.
const RESTART_POLL_INTERVAL_MS = 1000;
const RESTART_PROBE_DEADLINE_MS = 10_000;

export function SetupPage() {
  const [restartRequested, setRestartRequested] = useState(false);
  const [setupRestarting, setSetupRestarting] = useState(false);
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

  if (setupRestarting) {
    return <SetupRestartGuide />;
  }
  if (database.isPending) {
    return <main className="lux-state-screen" aria-busy="true"><div className="lux-spinner" aria-hidden="true" /><p>正在检查数据库配置</p></main>;
  }
  if (restartRequested) {
    return <RestartingScreen description="数据库迁移完成后会自动继续管理员初始化，请不要关闭此页面。" />;
  }
  if (database.error) {
    return <main className="lux-state-screen" role="alert"><h1>无法读取数据库配置</h1><p>{database.error.message}</p></main>;
  }
  if (database.data.restartRequired) {
    return <main className="lux-auth-screen"><section className="lux-auth-card lux-setup-card"><div className="lux-auth-brand"><LuxLogo className="lux-brand-logo" /><strong>Lux</strong></div><h1>请重启 Lux</h1><p>PostgreSQL 配置已经保存。重启 Lux 后，系统会运行迁移并继续管理员初始化。</p><LuxButton tone="primary" type="button" onClick={() => restart.mutate()} disabled={restart.isPending}>{restart.isPending ? "正在准备重启…" : "重启 Lux"}</LuxButton>{restart.error ? <p className="lux-error-copy" role="alert">{restart.error.message}</p> : <p className="lux-success-copy" role="status">重启期间页面会暂时无法访问，服务恢复后将自动继续。</p>}</section></main>;
  }
  if (!database.data.configured) {
    return <main className="lux-auth-screen"><DatabaseSetupPanel onSelected={() => undefined} /></main>;
  }
  return <main className="lux-auth-screen"><AdminSetupForm onRestarting={() => setSetupRestarting(true)} /></main>;
}

// Shown after setup completed while the process restarts. `setup/status`
// answers `initialized:true` as soon as the administrator row exists — which is
// already the case *before* the restart — so a single successful probe proves
// nothing: wait for a failed probe (the port is closed) and only accept the next
// successful one as "the restarted process is serving".
function SetupRestartGuide() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  useEffect(() => {
    let cancelled = false;
    let sawDisconnect = false;
    let elapsed = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const finish = () => {
      if (cancelled) return;
      cancelled = true;
      if (timer) clearTimeout(timer);
      // Setup is already a committed fact on the server, so flipping the cached
      // status lets App.tsx leave the setup route; the stale session probe is
      // dropped exactly like the non-restarting path does.
      queryClient.setQueryData(queryKeys.setup, { initialized: true });
      queryClient.removeQueries({ queryKey: queryKeys.me });
      navigate("/login", { replace: true });
    };

    const probe = async () => {
      if (cancelled) return;
      elapsed += RESTART_POLL_INTERVAL_MS;
      let up = false;
      try {
        await api.setupStatus();
        up = true;
      } catch {
        // A network error, or a 502/504 from the reverse proxy, both mean the
        // server is unreachable right now — that is the disconnect we wait for.
        up = false;
      }
      if (cancelled) return;
      if (up) {
        if (sawDisconnect) {
          finish();
          return;
        }
      } else {
        sawDisconnect = true;
      }
      if (!sawDisconnect && elapsed >= RESTART_PROBE_DEADLINE_MS) {
        finish();
        return;
      }
      timer = setTimeout(() => { void probe(); }, RESTART_POLL_INTERVAL_MS);
    };

    timer = setTimeout(() => { void probe(); }, RESTART_POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [navigate, queryClient]);

  return <RestartingScreen description="初始化已完成，Lux 正在重启以应用新配置。请不要关闭此页面，服务恢复后会自动进入登录页。" />;
}

// Shared with the database-switch flow above so both restarts look identical.
function RestartingScreen({ description }: { description: string }) {
  return <main className="lux-state-screen" role="status" aria-busy="true"><div className="lux-spinner" aria-hidden="true" /><h1>正在重启 Lux</h1><p>{description}</p></main>;
}
