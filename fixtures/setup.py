"""Make exactly one function incomplete in a copied fixture repository."""

import pathlib
import sys


if len(sys.argv) != 3:
    raise SystemExit("usage: setup.py <clamp|slugify|median> <repo>")

task, repo_arg = sys.argv[1:]
if task not in {"clamp", "slugify", "median"}:
    raise SystemExit(f"unknown task: {task}")

path = pathlib.Path(repo_arg) / "src" / "tiny_tasks.py"
source = path.read_text()
start_marker = f"    # fixture:{task}:start"
end_marker = f"    # fixture:{task}:end"
start = source.index(start_marker) + len(start_marker)
end = source.index(end_marker)
source = source[:start] + "\n    raise NotImplementedError\n" + source[end:]
path.write_text(source)
