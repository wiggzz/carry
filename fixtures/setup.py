"""Make the selected implementation incomplete in a copied fixture repository."""

import pathlib
import sys


TASK_FILES = {
    "clamp": ["src/tiny_tasks.py"],
    "slugify": ["src/tiny_tasks.py"],
    "median": ["src/tiny_tasks.py"],
    "release-plan": ["src/manifest.py", "src/planner.py"],
    "config-loader": ["src/config_merge.py", "src/config_loader.py"],
}


if len(sys.argv) != 3:
    raise SystemExit("usage: setup.py <task> <repo>")

task, repo_arg = sys.argv[1:]
if task not in TASK_FILES:
    raise SystemExit(f"unknown task: {task}")

for relative_path in TASK_FILES[task]:
    path = pathlib.Path(repo_arg) / relative_path
    source = path.read_text()
    start_marker = f"# fixture:{task}:start"
    end_marker = f"# fixture:{task}:end"
    marker_start = source.index(start_marker)
    start = marker_start + len(start_marker)
    end = source.index(end_marker, start)
    end_line_start = source.rfind("\n", start, end) + 1
    indentation = source[source.rfind("\n", 0, marker_start) + 1 : marker_start]
    source = (
        source[:start]
        + f"\n{indentation}raise NotImplementedError\n"
        + source[end_line_start:]
    )
    path.write_text(source)
