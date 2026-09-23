import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { motion } from "framer-motion";
import { FormEvent, type CSSProperties, useEffect, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { PluginLoginBackgroundResponse } from "../../lib/api/types";

const POSTER_WIDTH_PERCENT = 78;
const POSTER_COLUMN_COUNT = 5;
const POSTER_COLUMN_WIDTH_PERCENT = POSTER_WIDTH_PERCENT / 3;

export function LoginPage() {
  const queryClient = useQueryClient();
  const loginBackground = useQuery({
    queryKey: queryKeys.loginBackground,
    queryFn: () => api.loginBackground(),
    retry: false,
    staleTime: 5 * 60 * 1000,
  });
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const [failedBackgroundKey, setFailedBackgroundKey] = useState<string | null>(null);

  const login = useMutation({
    mutationFn: () => api.login(username, password),
    onSuccess: async () => {
      await queryClient.fetchQuery({
        queryKey: queryKeys.me,
        queryFn: () => api.me(),
        retry: false,
      });
    },
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    login.mutate();
  }

  const background = loginBackground.data;
  const pluginBackground = background && "contentKind" in background
    ? background as PluginLoginBackgroundResponse
    : undefined;
  const pluginItems = pluginBackground?.items ?? [];
  const pluginPosterItems = pluginBackground?.contentKind === "POSTER_FEED"
    ? pluginBackground.items.filter((item) => item.imageUrl.trim().length > 0)
    : [];
  const pluginHeroItem = pluginBackground?.contentKind === "HERO_IMAGE"
    ? pluginBackground.items[0]
    : undefined;
  const backgroundKey = [
    background?.source ?? "STATIC",
    ...(background?.source === "RECENTLY_ADDED"
      ? background.images
      : pluginItems.map((item) => item.imageUrl)),
  ].join("\n");
  useEffect(() => setFailedBackgroundKey(null), [backgroundKey]);
  const backgroundFailed = failedBackgroundKey === backgroundKey;
  const posterImages = backgroundFailed
    ? []
    : background?.source === "RECENTLY_ADDED"
      ? background.images.filter((image) => image.trim().length > 0)
      : pluginPosterItems.map((item) => item.imageUrl);
  const heroItem = backgroundFailed ? undefined : pluginHeroItem;
  const tmdbSource = background?.source === "PLUGIN:org.lux.tmdb-trending-background";
  const markBackgroundFailed = () => setFailedBackgroundKey(backgroundKey);
  const posterColumns = posterImages.reduce<Array<Array<{ image: string; index: number }>>>(
    (columns, image, index) => {
      columns[index % POSTER_COLUMN_COUNT].push({ image, index });
      return columns;
    },
    Array.from({ length: POSTER_COLUMN_COUNT }, () => []),
  );
  const posterColumnWidth = `${POSTER_COLUMN_WIDTH_PERCENT}%`;

  return (
    <main className="lux-auth-screen lux-auth-split-layout">
      {/* 顶部品牌标 (浮动在左上角，与效果图完全一致) */}
      <div className="lux-auth-brand-badge">
        <img
          className="lux-auth-brand-icon"
          src="/logo-white.svg"
          alt="Lux"
          width="34"
          height="34"
        />
        <span className="lux-auth-brand-text">Lux</span>
      </div>

      {/* 左侧海报艺术长卷展示区 */}
      <section className="lux-auth-visual" aria-label="登录页背景">
        {heroItem ? (
          <>
            <img
              className="lux-auth-hero-image"
              src={heroItem.imageUrl}
              alt=""
              aria-hidden="true"
              loading="eager"
              onError={markBackgroundFailed}
            />
            <div className="lux-auth-hero-shade" aria-hidden="true" />
            <div className="lux-auth-background-credit" aria-live="polite">
              <span>{heroItem.title ?? pluginBackground?.sourceName}</span>
              {heroItem.copyrightNotice ?? pluginBackground?.copyrightNotice
                ? <span>{heroItem.copyrightNotice ?? pluginBackground?.copyrightNotice}</span>
                : null}
            </div>
          </>
        ) : posterImages.length > 0 ? (
          <div
            className="lux-auth-poster-waterfall"
            style={{ "--lux-auth-poster-column-width": posterColumnWidth } as CSSProperties}
          >
            {posterColumns.map((column, columnIndex) => (
              <div className="lux-auth-poster-waterfall-column" key={`poster-column-${columnIndex}`}>
                {column.map(({ image, index }) => (
                  <img
                    key={`${image}-${index}`}
                    src={image}
                    alt=""
                    loading={index < 6 ? "eager" : "lazy"}
                    onError={markBackgroundFailed}
                  />
                ))}
              </div>
            ))}
          </div>
        ) : (
          <img
            className="lux-auth-poster-wall"
            src="/lux-poster-wall.jpg"
            alt=""
            loading="eager"
          />
        )}
        <div className="lux-auth-visual-fade" />
      </section>

      {tmdbSource ? (
        <details className="lux-auth-credits">
          <summary>关于与鸣谢</summary>
          <div className="lux-auth-credits-panel">
            <a href="https://www.themoviedb.org/" target="_blank" rel="noreferrer">
              <img
                className="lux-auth-tmdb-mark"
                src="https://www.themoviedb.org/assets/2/v4/logos/v2/blue_long_2-9665a76b1ae401a510ec1e0ca40ddcb3b0cfe45f1d51b77a308fea0845885648.svg"
                alt="TMDB"
                width="78"
                height="22"
                loading="lazy"
              />
              <span>The Movie Database</span>
            </a>
            <p>This product uses TMDB and the TMDB APIs but is not endorsed, certified, or otherwise approved by TMDB.</p>
          </div>
        </details>
      ) : null}

      {/* 右侧登录交互面板 */}
      <section className="lux-auth-panel">
        <motion.div
          className="lux-auth-card"
          initial={{ opacity: 0, y: 16 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.35 }}
        >
          <div className="lux-auth-header">
            <span className="lux-auth-eyebrow">欢迎探索 · WELCOME BACK</span>
            <h1 className="lux-auth-title">登录媒体中心</h1>
            <p className="lux-auth-desc">连接至您的私有 Lux 媒体服务器</p>
          </div>

          <form className="lux-auth-form" autoComplete="on" onSubmit={submit}>
            <div className="lux-auth-field">
              <label htmlFor="username">用户名</label>
              <input
                id="username"
                name="username"
                type="text"
                value={username}
                onChange={(event) => setUsername(event.target.value)}
                autoComplete="username"
                placeholder="请输入用户名"
                required
              />
            </div>

            <div className="lux-auth-field">
              <label htmlFor="password">密码</label>
              <div className="lux-auth-password-wrapper">
                <input
                  id="password"
                  name="password"
                  type={showPassword ? "text" : "password"}
                  value={password}
                  onChange={(event) => setPassword(event.target.value)}
                  autoComplete="current-password"
                  placeholder="••••••••"
                  required
                />
                <button
                  type="button"
                  className="lux-auth-password-toggle"
                  onClick={() => setShowPassword((v) => !v)}
                  aria-label={showPassword ? "隐藏密码" : "显示密码"}
                  title={showPassword ? "隐藏密码" : "显示密码"}
                >
                  <svg
                    className="lux-eye-icon"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.8"
                    aria-hidden="true"
                  >
                    {showPassword ? (
                      <>
                        <path d="M17.94 17.94A10.07 10.07 0 0112 20c-7 0-11-8-11-8a18.45 18.45 0 015.06-5.94M9.9 4.24A9.12 9.12 0 0112 4c7 0 11 8 11 8a18.5 18.5 0 01-2.16 3.19m-6.72-1.07a3 3 0 11-4.24-4.24" />
                        <line x1="1" y1="1" x2="23" y2="23" />
                      </>
                    ) : (
                      <>
                        <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
                        <circle cx="12" cy="12" r="3" />
                      </>
                    )}
                  </svg>
                </button>
              </div>
            </div>

            <button
              className="lux-button lux-button-large lux-auth-submit-btn"
              type="submit"
              disabled={login.isPending}
            >
              {login.isPending ? "正在进入…" : "进入影院 ›"}
            </button>
          </form>

          {login.error ? (
            <p className="lux-error-copy" role="alert">
              {login.error.message}
            </p>
          ) : null}
        </motion.div>
      </section>
    </main>
  );
}
