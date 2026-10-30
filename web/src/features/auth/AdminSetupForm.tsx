import { useMutation, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import { LuxLogo } from "../../components/LuxLogo";
import type { LibraryKind } from "../../lib/api/types";
import { LuxButton, LuxDisclosure, LuxSelectField, LuxTextField } from "../../components/ui";

export function AdminSetupForm({ onRestarting }: { onRestarting?: () => void } = {}) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [values, setValues] = useState({
    username: "",
    displayName: "",
    password: "",
    libraryName: "",
    libraryKind: "MIXED" as LibraryKind,
    libraryRoot: "",
  });
  const setup = useMutation({
    mutationFn: () =>
      api.setup(values),
    onSuccess: (result) => {
      // The server restarts a moment after this 201 (docs/SETUP-RESTART-DESIGN.md
      // §5.2). Caching initialized:true here would make App.tsx leave the setup
      // route immediately, so the restart notice would never be shown: hand the
      // rest of the flow to SetupPage, which owns the polling.
      if (result.restarting && onRestarting) {
        onRestarting();
        return;
      }
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
        <label htmlFor="setup-username">管理员用户名<LuxTextField id="setup-username" value={values.username} onChange={(event) => setValues({ ...values, username: event.target.value })} required /></label>
        <label htmlFor="setup-display-name">显示名称<LuxTextField id="setup-display-name" value={values.displayName} onChange={(event) => setValues({ ...values, displayName: event.target.value })} /></label>
        <label htmlFor="setup-password">管理员密码<LuxTextField id="setup-password" value={values.password} onChange={(event) => setValues({ ...values, password: event.target.value })} type="password" autoComplete="new-password" minLength={8} required /></label>
        <LuxDisclosure summary="可选：创建首个媒体库" className="lux-setup-optional-disclosure">
          <div className="lux-setup-optional">
            <label htmlFor="setup-library-name">
              媒体库名称
              <LuxTextField id="setup-library-name" value={values.libraryName} onChange={(event) => setValues({ ...values, libraryName: event.target.value })} />
            </label>
            <label htmlFor="setup-library-kind">
              媒体库类型
              <LuxSelectField
                id="setup-library-kind"
                value={values.libraryKind}
                onChange={(event) => setValues({ ...values, libraryKind: event.target.value as LibraryKind })}
              >
                <option value="MIXED">混合</option>
                <option value="MOVIE">电影</option>
                <option value="SERIES">剧集</option>
                <option value="HOMEVIDEOS">其他视频</option>
              </LuxSelectField>
            </label>
            <label htmlFor="setup-library-root">
              媒体库路径
              <LuxTextField id="setup-library-root" value={values.libraryRoot} onChange={(event) => setValues({ ...values, libraryRoot: event.target.value })} placeholder="例如 /media/movies" />
            </label>
          </div>
        </LuxDisclosure>
        <LuxButton tone="primary" size="large" type="submit" disabled={setup.isPending}>{setup.isPending ? "正在初始化…" : "完成初始化"}</LuxButton>
      </form>
      {setup.error ? <p className="lux-error-copy" role="alert">{setup.error.message}</p> : null}
    </section>
  );
}
