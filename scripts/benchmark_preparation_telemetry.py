#!/usr/bin/env python3
"""Emit small allowlisted preparation heartbeats; never emit log contents/URLs."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import time

MARKER = 'BENCHMARK_PREPARATION '
PHASES = {'preparing', 'complete', 'failed'}
STATES = {'skipped', 'running', 'completed', 'failed', 'incomplete'}
COUNT_KEYS = ('cached', 'published', 'failed', 'blocked_by_environment', 'pending')


def count(value):
    return value if type(value) is int and 0 <= value <= 100000 else 0


def snapshot(root: Path, elapsed_seconds: float = 0.0):
    event = {'schema': 'carry.preparation-heartbeat.v1', 'phase': 'unknown',
             'dependency_status': 'unknown', 'elapsed_seconds': round(elapsed_seconds, 3),
             'observed_unix_seconds': int(time.time()), 'collection_errors': 0,
             'log_count': 0, 'log_bytes': 0, 'latest_log_kind': 'none',
             'latest_log_activity': 'unknown', 'last_log_age_seconds': 0}
    event.update({key + '_count': 0 for key in COUNT_KEYS})
    checkpoint = root / 'preparation/preparation-attempt.json'
    try:
        if checkpoint.exists():
            if checkpoint.is_symlink() or checkpoint.stat().st_size > 2_000_000:
                raise ValueError('invalid checkpoint')
            data = json.loads(checkpoint.read_text())
            if not isinstance(data, dict):
                raise ValueError('invalid checkpoint')
            phase = data.get('phase')
            event['phase'] = phase if isinstance(phase, str) and phase in PHASES else 'unknown'
            counts = data.get('status_counts', {})
            if isinstance(counts, dict):
                event.update({key + '_count': count(counts.get(key)) for key in COUNT_KEYS})
            stages = data.get('stages', {})
            dependency = stages.get('dependency_build', {}) if isinstance(stages, dict) else {}
            status = dependency.get('status') if isinstance(dependency, dict) else None
            event['dependency_status'] = status if isinstance(status, str) and status in STATES else 'unknown'
    except (OSError, ValueError, TypeError):
        event['collection_errors'] += 1
    latest = None
    logs = root / 'preparation/build-logs'
    for kind in ('base', 'env', 'instances'):
        kind_root = logs / kind
        if kind_root.is_symlink():
            continue
        for directory, dirs, files in os.walk(kind_root, followlinks=False):
            dirs[:] = [name for name in dirs if not (Path(directory) / name).is_symlink()]
            if event['log_count'] >= 1000:
                break
            if 'build_image.log' not in files:
                continue
            path = Path(directory) / 'build_image.log'
            try:
                if path.is_symlink():
                    continue
                stat = path.stat()
                event['log_count'] += 1
                event['log_bytes'] += stat.st_size
                if latest is None or stat.st_mtime > latest[0]:
                    latest = (stat.st_mtime, path, kind)
            except OSError:
                event['collection_errors'] += 1
    if latest:
        modified, path, kind = latest
        event['latest_log_kind'] = kind
        event['last_log_age_seconds'] = round(max(0, time.time() - modified), 3)
        try:
            with path.open('rb') as stream:
                stream.seek(max(0, path.stat().st_size - 4096))
                text = stream.read(4096).decode('utf-8', errors='replace').lower()
            # Classification only. Untrusted log bytes never enter the event.
            for activity, pattern in (
                ('error', r'error:|traceback|assertion.*failed'),
                ('solving', r'solving environment|solving package'),
                ('downloading', r'downloading|connecting to|collecting package'),
                ('installing', r'installing|executing transaction'),
                ('building', r'building|compiling|gcc |g\+\+ '),
            ):
                if re.search(pattern, text):
                    event['latest_log_activity'] = activity
                    break
        except OSError:
            event['collection_errors'] += 1
    try:
        usage = shutil.disk_usage(root)
        event.update(disk_free_bytes=usage.free, disk_total_bytes=usage.total)
        event['load1'] = round(os.getloadavg()[0], 3)
        for line in Path('/proc/meminfo').read_text().splitlines():
            if line.startswith('MemAvailable:'):
                event['mem_available_bytes'] = int(line.split()[1]) * 1024
                break
    except (OSError, ValueError):
        event['collection_errors'] += 1
    return event


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--work', type=Path)  # Worker CLI compatibility; no contents read.
    parser.add_argument('--interval', type=int, default=60)
    parser.add_argument('--once', action='store_true')
    args = parser.parse_args()
    if not 1 <= args.interval <= 600:
        parser.error('interval must be 1..600 seconds')
    started = time.monotonic()
    while True:
        print(MARKER + json.dumps(snapshot(args.root, time.monotonic() - started), sort_keys=True), flush=True)
        if args.once:
            return 0
        time.sleep(args.interval)


if __name__ == '__main__':
    raise SystemExit(main())
