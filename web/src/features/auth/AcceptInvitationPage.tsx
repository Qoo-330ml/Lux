import { useLayoutEffect, useRef, useState, type FormEvent } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { LuxLogo } from "../../components/LuxLogo";
import { ApiError, api } from "../../lib/api/client";
import { LuxButton, LuxLinkButton, LuxTextField } from "../../components/ui";

function invitationErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 409) return "用户名或邮箱不可用，请修改用户名后重试；仍有问题请联系管理员。";
    if (error.status === 400) return "邀请链接已失效或账户资料无效，请核对后重试或联系管理员。";
    if (error.status === 413) return "填写内容过长，请缩短后重试。";
  }
  return "暂时无法创建账户，请稍后重试。";
}

export function AcceptInvitationPage() {
  const location = useLocation();
  const navigate = useNavigate();
  const sitePath = location.pathname.replace(/\/register\/invitation\/*$/, "");
  const loginPath = sitePath ? "/" + sitePath.replace(/^\/+/, "") + "/" : "/";
  const [token, setToken] = useState(() => {
    const tokens = new URLSearchParams(location.hash.slice(1)).getAll("token");
    return tokens.length === 1 && /^[A-Za-z0-9_-]{1,128}$/.test(tokens[0]) ? tokens[0] : "";
  });
  const [username, setUsername] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [password, setPassword] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [created, setCreated] = useState(false);
  const [error, setError] = useState("");
  const inFlight = useRef(false);

  useLayoutEffect(() => {
    if (location.hash) {
      navigate({ pathname: location.pathname, search: location.search, hash: "" }, { replace: true });
    }
  }, [location.hash, location.pathname, location.search, navigate]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!token || inFlight.current) return;
    inFlight.current = true;
    setSubmitting(true);
    setError("");
    try {
      await api.acceptUserInvitation({ token, username: username.trim(), displayName: displayName.trim(), password }, sitePath);
      setToken("");
      setCreated(true);
    } catch (cause) {
      setError(invitationErrorMessage(cause));
    } finally {
      setPassword("");
      inFlight.current = false;
      setSubmitting(false);
    }
  }

  return (
    <main className="lux-auth-screen">
      <section className="lux-auth-card" aria-labelledby="invitation-heading">
        <div className="lux-auth-brand"><LuxLogo className="lux-brand-logo" /><strong>Lux</strong></div>
        <h1 id="invitation-heading">{created ? "账户创建成功" : "创建 Lux 账户"}</h1>
        {created ? (
          <div className="lux-auth-form">
            <p role="status">现在可以使用新账户登录。</p>
            <LuxLinkButton tone="primary" href={loginPath}>前往登录</LuxLinkButton>
          </div>
        ) : !token ? (
          <div className="lux-auth-form">
            <p className="lux-error-copy" role="alert">邀请链接无效，请联系管理员重新发送。</p>
            <LuxLinkButton tone="secondary" href={loginPath}>返回登录</LuxLinkButton>
          </div>
        ) : (
          <>
            <p>设置账户信息，完成后即可登录。</p>
            <form className="lux-auth-form" onSubmit={(event) => void submit(event)} aria-busy={submitting} aria-describedby={error ? "invitation-error" : undefined}>
              <label htmlFor="invitation-username">
                用户名
                <LuxTextField id="invitation-username" name="username" autoComplete="username" required maxLength={128} value={username} disabled={submitting} onChange={(event) => setUsername(event.target.value)} />
              </label>
              <label htmlFor="invitation-display-name">
                显示名称（可选）
                <LuxTextField id="invitation-display-name" name="displayName" autoComplete="nickname" maxLength={128} value={displayName} disabled={submitting} onChange={(event) => setDisplayName(event.target.value)} />
              </label>
              <label htmlFor="invitation-password">
                密码
                <LuxTextField id="invitation-password" name="password" type="password" autoComplete="new-password" required maxLength={1024} value={password} disabled={submitting} onChange={(event) => setPassword(event.target.value)} />
              </label>
              <LuxButton tone="primary" type="submit" disabled={submitting}>{submitting ? "正在创建…" : "创建账户"}</LuxButton>
              {error ? <p id="invitation-error" className="lux-error-copy" role="alert">{error}</p> : null}
            </form>
          </>
        )}
      </section>
    </main>
  );
}
