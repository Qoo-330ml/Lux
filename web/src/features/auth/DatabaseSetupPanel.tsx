import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { DatabaseSetupInput, SetupDatabaseBackend } from "../../lib/api/types";

type DatabaseSetupPanelProps = {
  onSelected: (restartRequired: boolean) => void;
};

const sslModes = [
  ["prefer", "优先使用 SSL"],
  ["require", "必须使用 SSL"],
  ["disable", "禁用 SSL"],
  ["verify-ca", "验证 CA"],
  ["verify-full", "验证证书和主机名"],
] as const;

export function DatabaseSetupPanel({ onSelected }: DatabaseSetupPanelProps) {
  const queryClient = useQueryClient();
  const [backend, setBackend] = useState<SetupDatabaseBackend>("SQLITE");
  const [postgres, setPostgres] = useState({
    host: "127.0.0.1",
    port: "5432",
    database: "lux",
    username: "lux",
    password: "",
    sslMode: "prefer" as (typeof sslModes)[number][0],
  });
  const [tested, setTested] = useState(false);

  const input: DatabaseSetupInput = backend === "SQLITE"
    ? { backend: "SQLITE" }
    : {
        backend: "POSTGRESQL",
        host: postgres.host,
        port: Number(postgres.port),
        database: postgres.database,
        username: postgres.username,
        password: postgres.password,
        sslMode: postgres.sslMode,
      };
  const test = useMutation({
    mutationFn: () => api.testDatabase(input),
    onSuccess: () => setTested(true),
    onError: () => setTested(false),
  });
  const select = useMutation({
    mutationFn: () => api.selectDatabase(input),
    onSuccess: (result) => {
      queryClient.setQueryData(queryKeys.setupDatabase, {
        configured: true,
        backend: result.backend,
        currentBackend: "SQLITE",
        restartRequired: result.restartRequired,
      });
      onSelected(result.restartRequired);
    },
  });

  function updatePostgres(field: keyof typeof postgres, value: string) {
    setTested(false);
    setPostgres((current) => ({ ...current, [field]: value }));
  }

  return (
    <section className="lux-auth-card lux-setup-card">
      <div className="lux-auth-brand"><img className="lux-brand-logo" src="/logo.svg" alt="" aria-hidden="true" /><strong>Lux</strong></div>
      <h1>选择数据库</h1>
      <p>选择 Lux 保存用户、媒体索引、播放进度和后台任务的位置。安装后不能直接切换数据库。</p>
      <fieldset className="lux-setup-database-options">
        <legend>数据库后端</legend>
        <label><input type="radio" name="database-backend" checked={backend === "SQLITE"} onChange={() => { setBackend("SQLITE"); setTested(false); }} /> 内置 SQLite <small>适合单个 NAS 实例，无需额外服务。</small></label>
        <label><input type="radio" name="database-backend" checked={backend === "POSTGRESQL"} onChange={() => { setBackend("POSTGRESQL"); setTested(false); }} /> 外部 PostgreSQL <small>需要提前准备可访问的 PostgreSQL 数据库。</small></label>
      </fieldset>
      {backend === "POSTGRESQL" ? <div className="lux-setup-optional lux-setup-postgres-fields">
        <label htmlFor="setup-pg-host">主机<input id="setup-pg-host" value={postgres.host} onChange={(event) => updatePostgres("host", event.target.value)} required /></label>
        <label htmlFor="setup-pg-port">端口<input id="setup-pg-port" value={postgres.port} onChange={(event) => updatePostgres("port", event.target.value)} inputMode="numeric" required /></label>
        <label htmlFor="setup-pg-database">数据库名<input id="setup-pg-database" value={postgres.database} onChange={(event) => updatePostgres("database", event.target.value)} required /></label>
        <label htmlFor="setup-pg-username">用户名<input id="setup-pg-username" value={postgres.username} onChange={(event) => updatePostgres("username", event.target.value)} required /></label>
        <label htmlFor="setup-pg-password">密码<input id="setup-pg-password" value={postgres.password} onChange={(event) => updatePostgres("password", event.target.value)} type="password" autoComplete="new-password" /></label>
        <label htmlFor="setup-pg-ssl">SSL 模式<select id="setup-pg-ssl" value={postgres.sslMode} onChange={(event) => updatePostgres("sslMode", event.target.value)}>{sslModes.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>
      </div> : null}
      <div className="lux-setup-database-actions">
        <button className="lux-button lux-button-secondary" type="button" onClick={() => test.mutate()} disabled={test.isPending || select.isPending}>{test.isPending ? "正在测试…" : "测试连接"}</button>
        <button className="lux-button lux-button-primary" type="button" onClick={() => select.mutate()} disabled={!tested || select.isPending}>{select.isPending ? "正在保存…" : "使用此数据库"}</button>
      </div>
      {test.isSuccess ? <p className="lux-success-copy" role="status">连接测试成功，可以继续。</p> : null}
      {test.error ? <p className="lux-error-copy" role="alert">{test.error.message}</p> : null}
      {select.error ? <p className="lux-error-copy" role="alert">{select.error.message}</p> : null}
    </section>
  );
}
