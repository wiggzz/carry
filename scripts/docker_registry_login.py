#!/usr/bin/env python3
"""Authenticate Docker without placing the registry token in argv or logs."""
from __future__ import annotations

import json
import os
import pathlib
import subprocess
import sys


def login(auth_file: pathlib.Path, docker_config: pathlib.Path) -> None:
    try:
        payload = json.loads(auth_file.read_text(encoding="utf-8"))
        username = payload.get("username")
        token = payload.get("token")
        if not isinstance(username, str) or not username or "\x00" in username:
            raise ValueError("Docker Hub username is missing or invalid")
        if not isinstance(token, str) or not token or "\x00" in token:
            raise ValueError("Docker Hub token is missing or invalid")

        docker_config.mkdir(parents=True, exist_ok=True, mode=0o700)
        docker_config.chmod(0o700)
        environment = dict(os.environ, DOCKER_CONFIG=str(docker_config))
        result = subprocess.run(
            ["docker", "login", "--username", username, "--password-stdin"],
            input=token,
            text=True,
            capture_output=True,
            check=False,
            env=environment,
        )
        if result.returncode != 0:
            raise RuntimeError(f"Docker Hub login failed with status {result.returncode}")
    finally:
        auth_file.unlink(missing_ok=True)


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: docker_registry_login.py AUTH_FILE DOCKER_CONFIG", file=sys.stderr)
        return 2
    try:
        login(pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]))
    except (OSError, ValueError, RuntimeError, json.JSONDecodeError) as error:
        print(str(error), file=sys.stderr)
        return 1
    print("Docker Hub login succeeded")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
