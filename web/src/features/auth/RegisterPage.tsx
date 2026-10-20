import { useQuery } from "@tanstack/react-query";
import { useLayoutEffect, useRef, useState, type FormEvent } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { LuxLogo } from "../../components/LuxLogo";
import { ApiError, api } from "../../lib/api/client";

type RegistrationStatus = "editing" | "submitting" | "submitted";
type VerificationStatus = "pending" | "success" | "error" | "invalid";

function registrationErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 400) return "请检查邮箱和账户资料后重试。";
    if (error.status === 409) return "用户名或邮箱不可用，请修改后重试。";
    if (error.status === 413) return "填写内容过长，请缩短后重试。";
    if (error.status === 429) return "请求过于频繁，请稍后重试。";
    if (error.status === 503) return "注册服务暂时不可用，请稍后重试。";
  }
  return "暂时无法提交申请，请稍后重试。";
}

function verificationErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 400) return "验证链接无效或已失效，请重新打开验证邮件。";
    if (error.status === 429) return "验证请求过于频繁，请稍后重新打开验证邮件。";
    if (error.status === 503) return "验证服务暂时不可用，请稍后重新打开验证邮件。";
  }
  return "暂时无法完成邮箱验证，请稍后重新打开验证邮件。";
}

function readFragmentToken(hash: string): string | null {
  const values = new URLSearchParams(hash.startsWith("#") ? hash.slice(1) : hash).getAll("token");
  return values.length === 1 && /^[A-Za-z0-9_-]{1,128}$/.test(values[0]) ? values[0] : null;
}

export function RegisterPage() {
  const location = useLocation();
  const navigate = useNavigate();
  const pathname = location.pathname.replace(/\/+$/, "");
  const verificationRoute = pathname.endsWith("/register/verify-email");
  const sitePath = pathname.replace(/\/register(?:\/verify-email)?$/, "");
  const loginPath = sitePath ? `${sitePath}/` : "/";
  const registerPath = `${sitePath}/register`;
  const verificationToken = useRef<string | null>(null);
  const [hasVerificationToken] = useState(() => {
    verificationToken.current = verificationRoute ? readFragmentToken(location.hash) : null;
    return verificationToken.current !== null;
  });
  const verificationStarted = useRef(false);
  const [verificationStatus, setVerificationStatus] = useState<VerificationStatus>(
    verificationRoute ? (hasVerificationToken ? "pending" : "invalid") : "pending",
  );
  const [verificationError, setVerificationError] = useState("");
  const [registrationStatus, setRegistrationStatus] = useState<RegistrationStatus>("editing");
  const [email, setEmail] = useState("");
  const [username, setUsername] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [password, setPassword] = useState("");
  const [registrationError, setRegistrationError] = useState("");
  const submitInFlight = useRef(false);

  const registrationSettings = useQuery({
    queryKey: ["public-registration-settings", sitePath],
    queryFn: () => api.publicRegistrationSettings(sitePath),
    enabled: !verificationRoute,
    retry: false,
    staleTime: 30_000,
  });

  useLayoutEffect(() => {
    if (location.hash) {
      navigate({ pathname: location.pathname, search: location.search, hash: "" }, { replace: true });
    }
    if (!verificationRoute || verificationStarted.current) return;

    const token = verificationToken.current;
    verificationToken.current = null;
    if (!token) {
      setVerificationStatus("invalid");
      return;
    }

    verificationStarted.current = true;
    setVerificationStatus("pending");
    void api.verifyPublicUserRegistrationEmail({ token }, sitePath).then(() => {
      setVerificationStatus("success");
    }).catch((error: unknown) => {
      setVerificationError(verificationErrorMessage(error));
      setVerificationStatus("error");
    });
  }, [location.hash, location.pathname, location.search, navigate, sitePath, verificationRoute]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (submitInFlight.current || registrationStatus !== "editing") return;

    submitInFlight.current = true;
    setRegistrationStatus("submitting");
    setRegistrationError("");
    try {
      await api.submitPublicUserRegistration({
        email: email.trim(),
        username: username.trim(),
        displayName: displayName.trim() || undefined,
        password,
      }, sitePath);
      setRegistrationStatus("submitted");
    } catch (error) {
      setRegistrationError(registrationErrorMessage(error));
      setRegistrationStatus("editing");
    } finally {
      setPassword("");
      submitInFlight.current = false;
    }
  }

  if (verificationRoute) {
    return (
      <main className="lux-auth-screen">
        <section className="lux-auth-card" aria-labelledby="registration-verification-heading">
          <div className="lux-auth-brand"><LuxLogo className="lux-brand-logo" /><strong>Lux</strong></div>
          <h1 id="registration-verification-heading">
            {verificationStatus === "success" ? "邮箱验证成功" : "验证邮箱"}
          </h1>
          {verificationStatus === "pending" ? (
            <p role="status" aria-busy="true">正在验证邮箱…</p>
          ) : verificationStatus === "success" ? (
            <div className="lux-auth-form">
              <p className="lux-success-copy" role="status">邮箱验证成功，等待管理员批准。审核完成后即可登录。</p>
              <a className="lux-button lux-button-primary" href={loginPath}>返回登录</a>
            </div>
          ) : verificationStatus === "error" ? (
            <div className="lux-auth-form">
              <p className="lux-error-copy" role="alert">{verificationError}</p>
              <a className="lux-button lux-button-secondary" href={registerPath}>返回注册页面</a>
            </div>
          ) : (
            <div className="lux-auth-form">
              <p className="lux-error-copy" role="alert">验证链接无效或已失效，请重新打开验证邮件。</p>
              <a className="lux-button lux-button-secondary" href={registerPath}>返回注册页面</a>
            </div>
          )}
        </section>
      </main>
    );
  }

  return (
    <main className="lux-auth-screen">
      <section className="lux-auth-card" aria-labelledby="registration-heading">
        <div className="lux-auth-brand"><LuxLogo className="lux-brand-logo" /><strong>Lux</strong></div>
        <h1 id="registration-heading">
          {registrationStatus === "submitted" ? "申请已收到" : "创建 Lux 账户"}
        </h1>
        {registrationSettings.isPending ? (
          <p role="status" aria-busy="true">正在检查注册设置…</p>
        ) : registrationSettings.error ? (
          <div className="lux-auth-form">
            <p className="lux-error-copy" role="alert">注册状态暂时无法读取，请稍后重试。</p>
            <button
              className="lux-button lux-button-secondary"
              type="button"
              disabled={registrationSettings.isFetching}
              onClick={() => void registrationSettings.refetch()}
            >
              {registrationSettings.isFetching ? "正在重试…" : "重试"}
            </button>
          </div>
        ) : registrationSettings.data?.registrationMode !== "PUBLIC_EMAIL" ? (
          <div className="lux-auth-form">
            <p role="status">当前仅接受管理员邀请，请联系管理员获取邀请链接。</p>
            <a className="lux-button lux-button-secondary" href={loginPath}>返回登录</a>
          </div>
        ) : registrationStatus === "submitted" ? (
          <div className="lux-auth-form">
            <p className="lux-success-copy" role="status">
              若申请符合条件，我们会发送验证邮件。请前往邮箱验证；验证后仍需管理员批准。
            </p>
            <a className="lux-button lux-button-secondary" href={loginPath}>返回登录</a>
          </div>
        ) : (
          <>
            <p>提交申请后，请验证邮箱。邮箱验证通过后还需管理员批准。</p>
            <form
              className="lux-auth-form"
              onSubmit={(event) => void submit(event)}
              aria-busy={registrationStatus === "submitting"}
              aria-describedby={registrationError ? "registration-error" : undefined}
            >
              <label htmlFor="registration-email">
                邮箱
                <input
                  id="registration-email"
                  name="email"
                  type="email"
                  autoComplete="email"
                  maxLength={254}
                  required
                  value={email}
                  disabled={registrationStatus === "submitting"}
                  onChange={(event) => setEmail(event.target.value)}
                />
              </label>
              <label htmlFor="registration-username">
                用户名
                <input
                  id="registration-username"
                  name="username"
                  autoComplete="username"
                  maxLength={128}
                  required
                  value={username}
                  disabled={registrationStatus === "submitting"}
                  onChange={(event) => setUsername(event.target.value)}
                />
              </label>
              <label htmlFor="registration-display-name">
                显示名称（可选）
                <input
                  id="registration-display-name"
                  name="displayName"
                  autoComplete="nickname"
                  maxLength={128}
                  value={displayName}
                  disabled={registrationStatus === "submitting"}
                  onChange={(event) => setDisplayName(event.target.value)}
                />
              </label>
              <label htmlFor="registration-password">
                密码
                <input
                  id="registration-password"
                  name="password"
                  type="password"
                  autoComplete="new-password"
                  maxLength={1_024}
                  required
                  value={password}
                  disabled={registrationStatus === "submitting"}
                  onChange={(event) => setPassword(event.target.value)}
                />
              </label>
              <button
                className="lux-button lux-button-primary"
                type="submit"
                disabled={registrationStatus === "submitting"}
              >
                {registrationStatus === "submitting" ? "正在提交…" : "提交申请"}
              </button>
              {registrationError ? (
                <p id="registration-error" className="lux-error-copy" role="alert">{registrationError}</p>
              ) : null}
            </form>
          </>
        )}
      </section>
    </main>
  );
}
