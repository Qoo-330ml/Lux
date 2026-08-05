import { useMutation, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";

export function SetupPage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [values, setValues] = useState({ username: "", displayName: "", password: "", libraryName: "", libraryRoot: "" });
  const setup = useMutation({
    mutationFn: () => api.setup({ ...values, libraryKind: "MIXED" }),
    onSuccess: () => {
      queryClient.setQueryData(queryKeys.setup, { initialized: true });
      queryClient.removeQueries({ queryKey: queryKeys.me });
      navigate("/login", { replace: true });
    },
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setup.mutate();
  }

  return (
    <main className="lux-auth-screen">
      <section className="lux-auth-card lux-setup-card">
        <div className="lux-auth-brand"><img className="lux-brand-logo" src="/logo.svg" alt="" aria-hidden="true" /><strong>Lux</strong></div>
        <span className="lux-eyebrow">INITIALIZE YOUR SERVER</span>
        <h1>开始使用 Lux</h1>
        <p>创建首个服务器管理员，稍后可以继续配置媒体库。</p>
        <form className="lux-auth-form" onSubmit={submit}>
          <label>管理员用户名<input value={values.username} onChange={(event) => setValues({ ...values, username: event.target.value })} required /></label>
          <label>显示名称<input value={values.displayName} onChange={(event) => setValues({ ...values, displayName: event.target.value })} /></label>
            <label>管理员密码<input value={values.password} onChange={(event) => setValues({ ...values, password: event.target.value })} type="password" autoComplete="new-password" minLength={8} required /></label>
          <details><summary>可选：创建首个媒体库</summary><div className="lux-setup-optional"><label>媒体库名称<input value={values.libraryName} onChange={(event) => setValues({ ...values, libraryName: event.target.value })} /></label><label>媒体库路径<input value={values.libraryRoot} onChange={(event) => setValues({ ...values, libraryRoot: event.target.value })} placeholder="例如 /media/movies" /></label></div></details>
          <button className="lux-button lux-button-primary" type="submit" disabled={setup.isPending}>{setup.isPending ? "正在初始化…" : "完成初始化"}</button>
        </form>
        {setup.error ? <p className="lux-error-copy" role="alert">{setup.error.message}</p> : null}
      </section>
    </main>
  );
}
