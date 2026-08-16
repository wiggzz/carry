#!/usr/bin/env python3
"""Fail-closed adapter shared by the run-scoped agent images."""
import argparse
import json
import os
import pathlib
import shlex
import subprocess
import sys

parser = argparse.ArgumentParser()
parser.add_argument("command", choices=["run"])
parser.add_argument("--model", required=True)
parser.add_argument("--reasoning", required=True)
parser.add_argument("--prompt", required=True)
parser.add_argument("--output", required=True)
args = parser.parse_args()
if not os.environ.get("OPENAI_API_KEY"):
    parser.error("OPENAI_API_KEY is required")

output = pathlib.Path(args.output)
output.mkdir(parents=True, exist_ok=True)
template = os.environ.get("AGENT_COMMAND", "")
if not template:
    parser.error("agent command was not configured")
prompt_text = pathlib.Path(args.prompt).read_text(encoding="utf-8")
values = {
    "model": args.model,
    "reasoning": args.reasoning,
    "prompt": args.prompt,
    "prompt_text": prompt_text,
    "output": args.output,
}
command = [part.format(**values) for part in shlex.split(template)]
workspace = os.environ.get("BENCHMARK_WORKSPACE", "/workspace")
baseline = subprocess.check_output(
    ["git", "rev-parse", "HEAD"], cwd=workspace, text=True
).strip()

if os.environ.get("AGENT_METHOD") == "pi":
    config = pathlib.Path(os.environ["HOME"]) / ".pi" / "agent"
    config.mkdir(parents=True, exist_ok=True)
    models = {
        "providers": {
            "openai-benchmark": {
                "baseUrl": "https://api.openai.com/v1",
                "api": "openai-responses",
                "apiKey": "$OPENAI_API_KEY",
                "models": [{
                    "id": args.model,
                    "name": "Benchmark model",
                    "reasoning": True,
                    "input": ["text", "image"],
                    "contextWindow": 400000,
                    "maxTokens": 128000,
                    "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                }],
            }
        }
    }
    (config / "models.json").write_text(json.dumps(models) + "\n", encoding="utf-8")

trace_path = output / "trace.log"
with trace_path.open("w", encoding="utf-8") as trace:
    try:
        result = subprocess.run(
            command,
            cwd=workspace,
            stdout=trace,
            stderr=subprocess.STDOUT,
            check=False,
            timeout=int(os.environ.get("AGENT_TIMEOUT_SECONDS", "1200")),
        )
        returncode = result.returncode
    except subprocess.TimeoutExpired:
        trace.write("\nagent timed out\n")
        returncode = 124

patch_path = output / "final.patch"
subprocess.run(["git", "add", "-N", "--", "."], cwd=workspace, check=True)
with patch_path.open("wb") as patch:
    subprocess.run(
        ["git", "diff", "--binary", "--no-ext-diff", baseline],
        cwd=workspace,
        stdout=patch,
        check=True,
    )

secret = os.environ["OPENAI_API_KEY"].encode()
trace_path.write_bytes(trace_path.read_bytes().replace(secret, b"[REDACTED]"))
if secret in patch_path.read_bytes():
    patch_path.write_bytes(b"")
    (output / "secret-leak-blocked").write_text(
        "agent patch contained the API key\n", encoding="utf-8"
    )
    returncode = 86
sys.exit(returncode)
