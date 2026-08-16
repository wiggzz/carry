#!/usr/bin/env python3
"""Fail-closed adapter shared by the run-scoped agent images."""
import argparse
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
values = {"model": args.model, "reasoning": args.reasoning, "prompt": args.prompt, "output": args.output}
command = [part.format(**values) for part in shlex.split(template)]
with (output / "trace.log").open("w", encoding="utf-8") as trace:
    result = subprocess.run(command, cwd="/workspace", stdout=trace, stderr=subprocess.STDOUT, check=False)
with (output / "final.patch").open("wb") as patch:
    subprocess.run(["git", "diff", "--binary", "--no-ext-diff"], cwd="/workspace", stdout=patch, check=True)
sys.exit(result.returncode)
