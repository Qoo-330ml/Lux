import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { motion } from "framer-motion";
import { FormEvent, type CSSProperties, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";

const MIN_POSTER_COLUMN_WIDTH = 180;
const POSTER_COLUMN_GAP = 12;
const POSTER_HORIZONTAL_PADDING = 40;
const MIN_POSTER_COLUMNS = 3;
const MAX_POSTER_COLUMNS = 5;

function getPosterColumnCount(width: number) {
  const availableWidth = Math.max(0, width - POSTER_HORIZONTAL_PADDING);
  const columnCount = Math.floor(
    (availableWidth + POSTER_COLUMN_GAP) / (MIN_POSTER_COLUMN_WIDTH + POSTER_COLUMN_GAP),
  );
  return Math.min(MAX_POSTER_COLUMNS, Math.max(MIN_POSTER_COLUMNS, columnCount));
}

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
  const posterWaterfallRef = useRef<HTMLDivElement>(null);
  const [posterColumnCount, setPosterColumnCount] = useState(MIN_POSTER_COLUMNS);

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

  const posterImages = loginBackground.data?.source === "RECENTLY_ADDED"
    ? loginBackground.data.images.filter((image) => image.trim().length > 0)
    : [];
  useEffect(() => {
    const posterWaterfall = posterWaterfallRef.current;
    const ResizeObserverConstructor = typeof window !== "undefined" ? window.ResizeObserver : undefined;
    if (!posterWaterfall || !ResizeObserverConstructor) {
      return undefined;
    }

    const updateColumnCount = (width: number) => {
      const columnCount = getPosterColumnCount(width);
      setPosterColumnCount(columnCount);
    };
    const observer = new ResizeObserverConstructor(([entry]) => {
      updateColumnCount(entry?.contentRect.width ?? posterWaterfall.clientWidth);
    });

    updateColumnCount(posterWaterfall.clientWidth);
    observer.observe(posterWaterfall);
    return () => observer.disconnect();
  }, [posterImages.length]);

  const posterColumns = posterImages.reduce<Array<Array<{ image: string; index: number }>>>(
    (columns, image, index) => {
      columns[index % posterColumnCount].push({ image, index });
      return columns;
    },
    Array.from({ length: posterColumnCount }, () => []),
  );

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
      <section className="lux-auth-visual" aria-hidden="true">
        {posterImages.length > 0 ? (
          <div
            className="lux-auth-poster-waterfall"
            ref={posterWaterfallRef}
            style={{ "--lux-auth-poster-column-count": posterColumnCount } as CSSProperties}
          >
            {posterColumns.map((column, columnIndex) => (
              <div className="lux-auth-poster-waterfall-column" key={`poster-column-${columnIndex}`}>
                {column.map(({ image, index }) => (
                  <img
                    key={image}
                    src={image}
                    alt=""
                    loading={index < 6 ? "eager" : "lazy"}
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

          <footer className="lux-auth-footer-notice">
            支持 Lux &amp; Emby 媒体库 · 端到端安全连接
          </footer>
        </motion.div>
      </section>
    </main>
  );
}
