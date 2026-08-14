import { useMutation, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { LuxLogo } from "../../components/LuxLogo";

export function AdminSetupForm() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [values, setValues] = useState({
    username: "",
    displayName: "",
    password: "",
    libraryName: "",
    libraryRoot: "",
  });
  const setup = useMutation({
    mutationFn: () =>
      api.setup({ ...values, libraryKind: "MIXED" }),
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
    <section className="lux-auth-card lux-setup-card">
      <div className="lux-auth-brand"><LuxLogo className="lux-brand-logo" /><strong>Lux</strong></div>
      <h1>创建管理员</h1>
      <p>数据库已准备好。创建首个服务器管理员，稍后可以继续配置媒体库。</p>
      <form className="lux-auth-form" onSubmit={submit}>
        <label htmlFor="setup-username">管理员用户名<input id="setup-username" value={values.username} onChange={(event) => setValues({ ...values, username: event.target.value })} required /></label>
        <label htmlFor="setup-display-name">显示名称<input id="setup-display-name" value={values.displayName} onChange={(event) => setValues({ ...values, displayName: event.target.value })} /></label>
        <label htmlFor="setup-password">管理员密码<input id="setup-password" value={values.password} onChange={(event) => setValues({ ...values, password: event.target.value })} type="password" autoComplete="new-password" minLength={8} required /></label>
        <details><summary>可选：创建首个媒体库</summary><div className="lux-setup-optional"><label htmlFor="setup-library-name">媒体库名称<input id="setup-library-name" value={values.libraryName} onChange={(event) => setValues({ ...values, libraryName: event.target.value })} /></label><label htmlFor="setup-library-root">媒体库路径<input id="setup-library-root" value={values.libraryRoot} onChange={(event) => setValues({ ...values, libraryRoot: event.target.value })} placeholder="例如 /media/movies" /></label></div></details>
        <button className="lux-button lux-button-primary" type="submit" disabled={setup.isPending}>{setup.isPending ? "正在初始化…" : "完成初始化"}</button>
      </form>
      {setup.error ? <p className="lux-error-copy" role="alert">{setup.error.message}</p> : null}
    </section>
  );
}
