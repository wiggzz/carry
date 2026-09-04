#!/usr/bin/env python3
"""Render bounded EC2 user data for the SWE-bench worker."""
import argparse
import base64
from pathlib import Path


EC2_USER_DATA_MAX_BYTES = 16_384


def render_user_data(worker_script: str, bootstrap_config_url: str) -> str:
    """Embed one config capability and the audited worker implementation."""
    if not worker_script or "\x00" in worker_script:
        raise ValueError("worker script must be nonempty UTF-8 text without NUL bytes")
    if not bootstrap_config_url.startswith("https://"):
        raise ValueError("bootstrap config URL must use HTTPS")
    config_url_b64 = base64.b64encode(bootstrap_config_url.encode("utf-8")).decode("ascii")
    rendered = (
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        f"BOOTSTRAP_CONFIG_URL_B64={config_url_b64}\n"
        "# BEGIN CARRY EC2 WORKER\n"
        f"{worker_script}"
    )
    size = len(rendered.encode("utf-8"))
    if size > EC2_USER_DATA_MAX_BYTES:
        raise ValueError(f"EC2 user data is {size} bytes; maximum is {EC2_USER_DATA_MAX_BYTES}")
    return rendered


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--worker-script", type=Path, required=True)
    parser.add_argument("--bootstrap-config-url", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.write_text(
        render_user_data(
            args.worker_script.read_text(encoding="utf-8"), args.bootstrap_config_url
        ),
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
