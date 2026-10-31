#!/usr/bin/env python3
"""Build this checkout and run the standalone black-box suites (Unix)."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import importlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
SUITES = ('proxy_transparent_history', 'proxy_noodle_regressions')


def discover():
    sys.path.insert(0, str(ROOT / 'tests'))
    def cases(suite):
        for item in suite:
            if isinstance(item, unittest.TestSuite):
                yield from cases(item)
            else:
                yield item.id()
    result = []
    for name in SUITES:
        module = importlib.import_module(name)
        found = list(cases(unittest.defaultTestLoader.loadTestsFromModule(module)))
        if not found:
            raise RuntimeError(f'No integration cases found in {name}')
        result.extend(found)
    if len(result) != len(set(result)):
        raise RuntimeError('Duplicate integration cases')
    return sorted(result)


def build():
    # Cargo chooses the target directory and reports the actual rebuilt artifact;
    # never accept an arbitrary/stale CARRY_TEST_BINARY from the environment.
    result = subprocess.run(['cargo', 'build', '--locked', '--bin', 'carry',
                             '--message-format=json'], cwd=ROOT, text=True,
                            stdout=subprocess.PIPE, check=True)
    binaries = {entry['executable'] for line in result.stdout.splitlines()
                if (entry := json.loads(line)).get('reason') == 'compiler-artifact'
                and entry.get('executable') and entry['target']['name'] == 'carry'
                and 'bin' in entry['target']['kind']}
    if len(binaries) != 1:
        raise RuntimeError(f'Expected one Carry executable, got {binaries}')
    binary = Path(binaries.pop()).resolve(strict=True)
    if not os.access(binary, os.X_OK):
        raise RuntimeError(f'Carry executable is not executable: {binary}')
    return str(binary)


def run_case(case, binary, timeout=300):
    module, selector = case.split('.', 1)
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix='carry-case-') as evidence:
        env = {**os.environ, 'CARRY_TEST_BINARY': binary,
               'CARRY_TEST_EVIDENCE_DIR': evidence}
        process = subprocess.Popen([sys.executable, '-B',
                                    str(ROOT / 'tests' / (module + '.py')),
                                    selector, '-v'], cwd=ROOT, env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                   text=True, start_new_session=True)
        try:
            output, _ = process.communicate(timeout=timeout)
            code = process.returncode
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            output, _ = process.communicate()
            code = 124
            output += f'\nTimed out after {timeout}s\n'
    return case, code, time.monotonic() - started, output


def run(cases, binary, jobs):
    failed = 0
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = [pool.submit(run_case, case, binary) for case in cases]
        for future in as_completed(futures):
            case, code, elapsed, output = future.result()
            print(f'\n{case}: exit={code}, {elapsed:.2f}s\n{output}', flush=True)
            failed += code != 0
    print(f'{len(cases)} cases completed; {failed} failed', flush=True)
    return int(failed != 0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--list', action='store_true', help='list cases without building')
    parser.add_argument('--jobs', type=int, choices=range(1, 9), default=2)
    args = parser.parse_args()
    cases = discover()
    print('\n'.join(cases), flush=True)
    if args.list:
        return 0
    return run(cases, build(), args.jobs)


if __name__ == '__main__':
    sys.exit(main())
