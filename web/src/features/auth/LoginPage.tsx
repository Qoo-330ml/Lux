import { useMutation, useQueryClient } from "@tanstack/react-query";
import { motion } from "framer-motion";
import { FormEvent, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { LuxLogo } from "../../components/LuxLogo";

export function LoginPage() {
  const queryClient = useQueryClient();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const login = useMutation({
    mutationFn: () => api.login(username, password),
    onSuccess: (user) => {
      queryClient.setQueryData(queryKeys.me, { user });
    },
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    login.mutate();
  }

  return (
    <main className="lux-auth-screen">
      <div className="lux-auth-backdrop" />
      <motion.section className="lux-auth-card" initial={{ opacity: 0, y: 18 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.35 }}>
        <div className="lux-auth-brand"><LuxLogo className="lux-brand-logo" /><strong>Lux</strong></div>
        <h1>欢迎回到 Lux</h1>
        <p>进入你的私人电影空间。</p>
        <form className="lux-auth-form" onSubmit={submit}>
          <label>用户名<input value={username} onChange={(event) => setUsername(event.target.value)} autoComplete="username" required /></label>
          <label>密码<input value={password} onChange={(event) => setPassword(event.target.value)} type="password" autoComplete="current-password" required /></label>
          <button className="lux-button lux-button-primary" type="submit" disabled={login.isPending}>{login.isPending ? "正在进入…" : "进入 Lux"}</button>
        </form>
        {login.error ? <p className="lux-error-copy" role="alert">{login.error.message}</p> : null}
      </motion.section>
    </main>
  );
}
