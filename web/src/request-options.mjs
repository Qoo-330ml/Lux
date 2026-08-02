export function requestOptions(options = {}) {
  const headers = { Accept: "application/json", ...(options.headers || {}) };
  if (options.body) headers["Content-Type"] = "application/json";
  return { ...options, credentials: "same-origin", headers };
}
