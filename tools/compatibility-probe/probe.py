#!/usr/bin/env python3
"""Run a redacted local Emby compatibility smoke probe.

The probe intentionally emits only request paths, status codes, and a small
response shape. Passwords, access tokens, cookies, and arbitrary response
values never enter the output document.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


class ProbeError(RuntimeError):
    """Raised when the compatibility sequence cannot be completed."""


_SENSITIVE_FIELD_NAMES = {
    "AccessToken",
    "ApiKey",
    "Cookie",
    "Password",
    "Pw",
    "Token",
}


@dataclass(frozen=True)
class HttpResult:
    status: int
    body: Any


def _response_summary(path: str, body: Any) -> dict[str, Any]:
    if isinstance(body, list):
        return {"type": "list", "count": len(body)}
    if not isinstance(body, dict):
        return {"type": type(body).__name__}

    summary: dict[str, Any] = {
        "fields": sorted(key for key in body if key not in _SENSITIVE_FIELD_NAMES)
    }
    return summary


def record_event(method: str, path: str, status: int, body: Any) -> dict[str, Any]:
    """Return a stable, value-redacted event suitable for a fixture."""

    return {
        "method": method,
        "path": path,
        "status": status,
        "response": _response_summary(path, body),
    }


def _decode_body(raw: bytes) -> Any:
    if not raw:
        return None
    try:
        return json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return {"type": "non-json"}


def _request(base_url: str, method: str, path: str, *, headers: dict[str, str] | None = None, payload: dict[str, Any] | None = None) -> HttpResult:
    body = None
    request_headers = {"Accept": "application/json"}
    if headers:
        request_headers.update(headers)
    if payload is not None:
        body = json.dumps(payload).encode("utf-8")
        request_headers["Content-Type"] = "application/json"

    request = Request(
        f"{base_url.rstrip('/')}{path}",
        data=body,
        headers=request_headers,
        method=method,
    )
    try:
        with urlopen(request, timeout=10) as response:
            return HttpResult(response.status, _decode_body(response.read()))
    except HTTPError as error:
        return HttpResult(error.code, _decode_body(error.read()))
    except (URLError, TimeoutError, OSError) as error:
        raise ProbeError(f"request failed: {type(error).__name__}") from error


def run_probe(base_url: str, username: str, password: str) -> list[dict[str, Any]]:
    """Run the login/lifecycle sequence and return only redacted events."""

    events: list[dict[str, Any]] = []

    def call(method: str, path: str, **kwargs: Any) -> HttpResult:
        result = _request(base_url, method, path, **kwargs)
        events.append(record_event(method, path, result.status, result.body))
        return result

    public_info = call("GET", "/System/Info/Public")
    if public_info.status != 200:
        raise ProbeError("public system information did not return 200")

    public_users = call("GET", "/Users/Public")
    if public_users.status != 200:
        raise ProbeError("public users did not return 200")

    login = call(
        "POST",
        "/Users/AuthenticateByName",
        headers={
            "Authorization": "Emby Client=Lux compatibility probe, Device=local ARM, DeviceId=lux-probe, Version=0.1.0"
        },
        payload={"Username": username, "Pw": password},
    )
    if login.status != 200 or not isinstance(login.body, dict):
        raise ProbeError("authentication did not return 200")

    access_token = login.body.get("AccessToken")
    if not isinstance(access_token, str) or not access_token:
        raise ProbeError("authentication response did not contain an access token")

    token_headers = {"X-Emby-Token": access_token}
    protected_info = call("GET", "/System/Info", headers=token_headers)
    if protected_info.status != 200:
        raise ProbeError("authenticated system information did not return 200")

    ping = call("GET", "/System/Ping", headers=token_headers)
    if ping.status != 200:
        raise ProbeError("authenticated system ping did not return 200")

    logout = call("POST", "/Sessions/Logout", headers=token_headers)
    if logout.status != 204:
        raise ProbeError("logout did not return 204")

    after_logout = call("GET", "/System/Info", headers=token_headers)
    if after_logout.status != 401:
        raise ProbeError("revoked token did not return 401")

    return events


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:18099")
    parser.add_argument("--username", required=True)
    parser.add_argument(
        "--password",
        default=None,
        help="temporary local test password; prefer LUX_PROBE_PASSWORD",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    password = args.password or os.environ.get("LUX_PROBE_PASSWORD")
    if not password:
        print("set LUX_PROBE_PASSWORD or pass --password", file=sys.stderr)
        return 2

    try:
        events = run_probe(args.base_url, args.username, password)
    except ProbeError as error:
        print(json.dumps({"ok": False, "error": str(error)}, ensure_ascii=False))
        return 1

    print(json.dumps({"ok": True, "events": events}, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
