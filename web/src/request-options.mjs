const csrfCookie = "lux_csrf";
const csrfTokenStorageKey = "lux_csrf_token";

function readCookie(name) {
  if (typeof document === "undefined") return "";
  const value = document.cookie
    .split("; ")
    .find((part) => part.startsWith(`${name}=`));
  if (!value) return "";
  try {
    return decodeURIComponent(value.slice(name.length + 1));
  } catch {
    return "";
  }
}

function writeClientCookie(value, maxAge) {
  if (typeof document === "undefined") return;
  const maxAgeAttribute = maxAge === undefined ? "" : ` Max-Age=${maxAge};`;
  const secureAttribute = typeof window !== "undefined" && window.location.protocol === "https:"
    ? "; Secure"
    : "";
  document.cookie = `${csrfCookie}=${encodeURIComponent(value)}; Path=/;${maxAgeAttribute} SameSite=Lax${secureAttribute}`;
}

export function readCsrfToken() {
  try {
    if (typeof localStorage !== "undefined") {
      const stored = localStorage.getItem(csrfTokenStorageKey);
      if (stored) return stored;
    }
  } catch {
    // Private browsing and restrictive storage policies must not break requests.
  }
  return readCookie(csrfCookie);
}

export function rememberCsrfToken(token) {
  if (typeof token !== "string" || !token) return;
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(csrfTokenStorageKey, token);
    }
  } catch {
    // The Cookie fallback remains available when client storage is blocked.
  }
  writeClientCookie(token);
}

export function clearCsrfToken() {
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.removeItem(csrfTokenStorageKey);
    }
  } catch {
    // There is nothing else to do when client storage is unavailable.
  }
  writeClientCookie("", 0);
}

export function requestOptions(options = {}) {
  const headers = { Accept: "application/json", ...(options.headers || {}) };
  const method = options.method?.toUpperCase() ?? "GET";
  const hasCsrfHeader = Object.keys(headers).some((key) => key.toLowerCase() === "x-csrf-token");
  if (method !== "GET" && method !== "HEAD" && !hasCsrfHeader) {
    const csrf = readCsrfToken();
    if (csrf) headers["x-csrf-token"] = csrf;
  }
  if (options.body) headers["Content-Type"] = "application/json";
  return { ...options, credentials: "same-origin", headers };
}
