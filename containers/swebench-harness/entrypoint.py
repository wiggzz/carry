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
parser.add_argument("--harness", choices=["carry", "codex", "pi"],
                    default=os.environ.get("AGENT_HARNESS"))
parser.add_argument("--model", required=True)
parser.add_argument("--reasoning", required=True)
parser.add_argument("--prompt", required=True)
parser.add_argument("--output", required=True)
parser.add_argument("--resume-session", type=pathlib.Path)
parser.add_argument("--codex-session", type=pathlib.Path)
parser.add_argument("--codex-thread")
parser.add_argument("--pi-session-dir", type=pathlib.Path)
args = parser.parse_args()
if args.resume_session and args.harness != "carry":
    parser.error("--resume-session is supported only by the Carry harness")
if (args.codex_session or args.codex_thread) and args.harness != "codex":
    parser.error("--codex-session and --codex-thread are supported only by Codex")
if args.codex_thread and not args.codex_session:
    parser.error("--codex-thread requires --codex-session")
if args.pi_session_dir and args.harness != "pi":
    parser.error("--pi-session-dir is supported only by the Pi harness")
if not os.environ.get("OPENAI_API_KEY"):
    parser.error("OPENAI_API_KEY is required")
if not os.environ.get("OPENAI_BASE_URL"):
    parser.error("OPENAI_BASE_URL is required")
compaction_policy = os.environ.get("CARRY_COMPACTION_POLICY", "economic")
if compaction_policy not in {"economic", "disabled"}:
    parser.error("CARRY_COMPACTION_POLICY must be economic or disabled")
keep_lease_turns = os.environ.get("CARRY_KEEP_LEASE_TURNS", "")
if keep_lease_turns and (not keep_lease_turns.isascii() or not keep_lease_turns.isdecimal()
                         or int(keep_lease_turns) < 1):
    parser.error("CARRY_KEEP_LEASE_TURNS must be a positive ASCII decimal integer")
payoff_requests = os.environ.get("CARRY_COMPACTION_PAYOFF_REQUESTS", "1")
if (not payoff_requests.isascii() or not payoff_requests.isdecimal()
        or int(payoff_requests) < 1):
    parser.error("CARRY_COMPACTION_PAYOFF_REQUESTS must be a positive ASCII decimal integer")

output = pathlib.Path(args.output)
output.mkdir(parents=True, exist_ok=True)
template = os.environ.get("AGENT_COMMAND", "")
if not template:
    if not args.harness:
        parser.error("agent command or --harness is required")
    root = pathlib.Path(os.environ.get("PREPARED_HARNESS_ROOT", "/opt/swebench-harness"))
    template = {
        "carry": f"{root}/bin/carry --cwd /testbed --session-dir {{output}} "
                 "--model {model} --compaction-policy {compaction_policy} -p {prompt_text}",
        "codex": f"{root}/bin/codex exec --dangerously-bypass-approvals-and-sandbox "
                 "--model {model} --config model_reasoning_effort={reasoning} "
                 "--json {prompt_text}",
        "pi": f"{root}/bin/pi --mode json --provider openai-benchmark --model {{model}} "
              "--thinking {reasoning} --no-session {prompt_text}",
    }[args.harness]
prompt_text = pathlib.Path(args.prompt).read_text(encoding="utf-8")
values = {
    "model": args.model,
    "reasoning": args.reasoning,
    "compaction_policy": compaction_policy,
    "prompt": args.prompt,
    "prompt_text": prompt_text,
    "output": args.output,
}
command = [part.format(**values) for part in shlex.split(template)]
if args.harness == "carry" and keep_lease_turns:
    command.extend(["--keep-lease-turns", keep_lease_turns])
if args.harness == "carry":
    command.extend(["--compaction-payoff-requests", payoff_requests])
if args.resume_session:
    command.extend(["--resume", str(args.resume_session)])
if args.codex_session and args.codex_thread:
    codex_binary = str(
        pathlib.Path(os.environ.get("PREPARED_HARNESS_ROOT", "/opt/swebench-harness")) / "bin" / "codex"
    ) if not os.environ.get("AGENT_COMMAND") else "codex"
    command = [
        codex_binary, "exec", "resume", "--model", args.model,
        "--config", f"model_reasoning_effort={args.reasoning}",
        "--dangerously-bypass-approvals-and-sandbox", "--json",
        args.codex_thread, prompt_text,
    ]
if args.harness == "pi":
    if args.pi_session_dir:
        command.remove("--no-session")
        command.extend([
            "--session", str(args.pi_session_dir / "session.jsonl"),
            "--session-dir", str(args.pi_session_dir),
        ])
    else:
        command.append("--no-session")
workspace = os.environ.get("BENCHMARK_WORKSPACE", "/workspace")
subprocess.run(
    ["git", "config", "--global", "--add", "safe.directory", workspace],
    check=True,
)
baseline = subprocess.check_output(
    ["git", "rev-parse", "HEAD"], cwd=workspace, text=True
).strip()

if args.harness == "pi":
    config = pathlib.Path(os.environ["HOME"]) / ".pi" / "agent"
    config.mkdir(parents=True, exist_ok=True)
    models = {
        "providers": {
            "openai-benchmark": {
                "baseUrl": os.environ["OPENAI_BASE_URL"],
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
agent_env = os.environ.copy()
if args.codex_session:
    agent_env["CODEX_HOME"] = str(args.codex_session)
with trace_path.open("w", encoding="utf-8") as trace:
    returncode = 0
    if args.harness == "codex":
        try:
            default_codex = str(
                pathlib.Path(os.environ.get("PREPARED_HARNESS_ROOT", "/opt/swebench-harness"))
                / "bin" / "codex"
            ) if not os.environ.get("AGENT_COMMAND") else "codex"
            login = subprocess.run(
                [os.environ.get("CODEX_BINARY", default_codex), "login", "--with-api-key"],
                input=os.environ["OPENAI_API_KEY"],
                cwd=workspace,
                env=agent_env,
                stdout=trace,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
                timeout=30,
            )
            returncode = login.returncode
            if returncode == 0:
                codex_home = pathlib.Path(agent_env.get("CODEX_HOME", str(pathlib.Path(os.environ["HOME"]) / ".codex")))
                codex_home.mkdir(parents=True, exist_ok=True)
                (codex_home / "config.toml").write_text(
                    'model_provider = "openai-benchmark"\n\n'
                    '[model_providers.openai-benchmark]\n'
                    'name = "OpenAI benchmark proxy"\n'
                    f'base_url = {json.dumps(os.environ["OPENAI_BASE_URL"])}\n'
                    'wire_api = "responses"\n'
                    'requires_openai_auth = true\n'
                    'supports_websockets = false\n',
                    encoding="utf-8",
                )
                # Codex now reads its run-scoped auth file from the tmpfs HOME.
                # Do not expose the key to model-controlled child shell commands.
                agent_env.pop("OPENAI_API_KEY", None)
        except subprocess.TimeoutExpired:
            trace.write("\ncodex login timed out\n")
            returncode = 124
    if returncode == 0:
        try:
            result = subprocess.run(
                command,
                cwd=workspace,
                env=agent_env,
                stdout=trace,
                stderr=subprocess.STDOUT,
                check=False,
                timeout=int(os.environ.get("AGENT_TIMEOUT_SECONDS", "1200")),
            )
            returncode = result.returncode
        except subprocess.TimeoutExpired:
            trace.write("\nagent timed out\n")
            returncode = 124

if args.codex_session:
    # Login writes credentials below CODEX_HOME; the retained artifact must contain
    # native rollout state only, never reusable auth material.
    (args.codex_session / "auth.json").unlink(missing_ok=True)

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
