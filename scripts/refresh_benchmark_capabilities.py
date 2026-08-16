#!/usr/bin/env python3
"""Rotate opaque S3 capabilities for a long-running disposable benchmark worker."""
from __future__ import annotations

import json
import os
import pathlib
import sys
import time
import urllib.parse
import urllib.request
from collections.abc import Callable
from typing import Any


def _validated_s3_url(value: Any) -> str:
    if not isinstance(value, str):
        raise ValueError("capability URL must be a string")
    parsed = urllib.parse.urlparse(value)
    hostname = parsed.hostname or ""
    if parsed.scheme != "https" or not (
        hostname.endswith(".amazonaws.com") or hostname.endswith(".amazonaws.com.cn")
    ):
        raise ValueError("capability URL must use an AWS HTTPS endpoint")
    return value


def rotate_once(
    control_url: str,
    result_url_file: pathlib.Path,
    *,
    fetch: Callable[..., Any] = urllib.request.urlopen,
) -> str:
    _validated_s3_url(control_url)
    request = urllib.request.Request(control_url, method="GET")
    with fetch(request, timeout=30) as response:
        payload = json.loads(response.read())
    next_control = _validated_s3_url(payload.get("control_get_url"))
    result_put = _validated_s3_url(payload.get("result_put_url"))
    result_url_file.parent.mkdir(parents=True, exist_ok=True)
    temporary = result_url_file.with_name(result_url_file.name + ".tmp")
    try:
        temporary.write_text(result_put, encoding="utf-8")
        temporary.chmod(0o600)
        os.replace(temporary, result_url_file)
    finally:
        temporary.unlink(missing_ok=True)
    return next_control


def main() -> int:
    control_url = os.environ.get("CONTROL_GET_URL", "")
    result_url_file = pathlib.Path(os.environ.get("RESULT_URL_FILE", "/dev/shm/carry-result-url"))
    interval = int(os.environ.get("CAPABILITY_REFRESH_SECONDS", "120"))
    if interval < 30 or interval > 600:
        raise SystemExit("CAPABILITY_REFRESH_SECONDS must be between 30 and 600")
    while True:
        try:
            control_url = rotate_once(control_url, result_url_file)
        except Exception as error:  # keep the prior unexpired capability on transient failures
            print(f"capability refresh failed: {type(error).__name__}", file=sys.stderr)
        time.sleep(interval)


if __name__ == "__main__":
    raise SystemExit(main())
