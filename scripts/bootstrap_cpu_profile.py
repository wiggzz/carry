#!/usr/bin/env python3
"""Default-off hosted CPU-IP samples; no stacks, privilege escalation or repair."""
from pathlib import Path
import argparse
import hashlib
import json
import math
import os
import re
import sys
import bootstrap_diagnostic as diagnostic

APPROVED_HELPER = 'a23268b5069a79776d211844ee9d71e7b187511636b9fcb86354be7f0160d9a9'
TOOL_ROOTS = (Path('/usr/lib'), Path('/usr/bin'))
DPKG = '/usr/bin/dpkg-query'
NM = '/usr/bin/nm'


def sha256(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(262144), b''):
            value.update(chunk)
    return value.hexdigest()


def elf(path):
    with Path(path).open('rb') as stream:
        return stream.read(4) == b'\x7fELF'


def clean_environment(private, carry=None):
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(private / 'home'),
           'XDG_CONFIG_HOME': str(private / 'home/config'),
           'XDG_CACHE_HOME': str(private / 'home/cache'),
           'XDG_DATA_HOME': str(private / 'home/data'), 'TMPDIR': str(private / 'tmp'),
           'LC_ALL': 'C', 'LANG': 'C', 'PERF_CONFIG': '/dev/null',
           'PERF_BUILDID_DIR': str(private / 'buildid'), 'DEBUGINFOD_URLS': '',
           'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null'}
    if carry is not None:
        env['CARRY_TEST_BINARY'] = str(carry)
    return env



def report_loss(text):
    # Only the exact supported perf receipt is trustworthy; absent/ambiguous
    # metadata is unknown, never a zero-loss default.
    headers = [line for line in text.splitlines() if 'lost' in line.lower() and line.lstrip().startswith('#')]
    match = re.fullmatch(r'# Total Lost Samples: ([0-9]+)', headers[0]) if len(headers) == 1 else None
    return int(match[1]) if match and int(match[1]) <= 18446744073709551615 else None


def report_samples(text):
    # convert_unit's blank-unit form is exact. Scaled K/M/G counts are
    # rounded, not an exact coverage receipt; unsupported forms stay unknown.
    headers = [line for line in text.splitlines() if line.lstrip().startswith('#')
               and 'samples' in line.lower() and 'lost' not in line.lower()]
    periods = [line for line in text.splitlines() if line.lstrip().startswith('#') and 'event count' in line.lower()]
    if periods:
        period = re.fullmatch(r'# Event count \(approx\.\): ([0-9]+)', periods[0]) if len(periods) == 1 else None
        if not period or not 0 < int(period[1]) <= 18446744073709551615:
            return None
    match = re.fullmatch(r"# Samples: ([0-9]+)  of event 'cpu-clock:u'", headers[0]) if len(headers) == 1 else None
    return int(match[1]) if match and 0 < int(match[1]) <= 1000000 else None


def symbol_rows(text, symbols, comms):
    """Only ELF-defined public names survive; unknown comm/symbol never copied."""
    groups = {}
    input_rows = 0
    hundredths = 0
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        fields = line.split('\t')
        if len(fields) != 4:
            raise ValueError('report schema')
        overhead, count, comm, symbol = (f.strip() for f in fields)
        if not re.fullmatch(r'[0-9]+\.[0-9]{2}%', overhead) or not count.isdecimal():
            raise ValueError('report numbers')
        input_rows += 1
        hundredths += int(overhead[:-1].replace('.', ''))
        overhead = float(overhead[:-1]); count = int(count)
        if not math.isfinite(overhead) or not 0 <= overhead <= 100 or not 0 <= count <= 1000000:
            raise ValueError('report range')
        comm = comm if comm in comms else '[other]'
        symbol = symbol[4:] if symbol.startswith('[.] ') else ''
        symbol = symbol if symbol in symbols and len(symbol) <= 512 else '[unknown-or-not-allowlisted]'
        row = groups.setdefault((comm, symbol), {'comm': comm, 'symbol': symbol,
                               'overhead_percent': 0.0, 'sample_count': 0})
        row['overhead_percent'] = round(row['overhead_percent'] + overhead, 6)
        row['sample_count'] += count
    # perf prints each absolute self-overhead to two decimals. Each input
    # contributes at most 0.005 percentage points of rounding error. Compare
    # exact integer hundredths: |sum - 100| <= input_rows * 0.005. Never
    # renormalize known symbols or remove unknown/Python/Tokio period mass.
    if not input_rows or abs(hundredths - 10000) * 2 > input_rows:
        raise ValueError('report absolute mass')
    rows = sorted(groups.values(), key=lambda r: (-r['overhead_percent'], r['comm'], r['symbol']))
    if len(rows) > 200:
        raise ValueError('report row budget')
    return rows


def record_command(perf, data, workload):
    # PERF_CONFIG=/dev/null; absence of -g means no callchain/stack capture.
    return ['/usr/bin/timeout', '--kill-after=2s', '30s', str(perf), 'record',
            '-e', 'cpu-clock:u', '-F', '99', '--strict-freq',
            '--no-buildid-cache', '-m', '128', '--max-size=16M',
            '-o', str(data), '--', *workload]


def original_case(root, private):
    # The inherited original body and its 5s socket deadline execute ONCE.
    code = 1
    try:
        code = diagnostic.inner(root, private / 'trace')
        return code
    finally:
        diagnostic.dump(private / 'case-exit.json', {'exit_code': code})


def hosted(root, manifest_path, private, report, install_status):
    if (os.environ.get('GITHUB_ACTIONS') != 'true' or
            os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted' or os.geteuid() == 0):
        raise RuntimeError('authorized non-root GitHub-hosted runner required')
    root = Path(root).resolve(); private = Path(private).absolute(); report = Path(report).absolute()
    temp = Path(os.environ['RUNNER_TEMP']).resolve()
    # Both raw data and every supervisor output remain in this private sibling,
    # NEVER inside bootstrap-latency/ (which the older workflow uploads whole).
    if (private != temp / 'bootstrap-cpu-private' or private.exists() or
            report != temp / 'bootstrap-cpu-symbols.json' or report.exists() or
            Path(manifest_path).resolve() != temp / 'bootstrap-latency/manifest.json' or
            Path(install_status).resolve() != temp / 'bootstrap-perf-install-exit'):
        raise RuntimeError('fresh runner-temp paths required')
    private.mkdir(mode=0o700)
    for name in ('home', 'tmp', 'buildid'):
        (private / name).mkdir(mode=0o700)
    env = clean_environment(private)
    evidence = {'schema': 1, 'status': 'SETUP_BLOCKED', 'stage': 'install',
                'frequency_hz': 99, 'socket_deadline_seconds': 5,
                'profile_wall_seconds': 30, 'root_cause_proven': 0,
                'profile_observer_effect_unmeasured': 1, 'is_original_red': 0,
                'case_exit_code': None, 'record_exit_code': None, 'rows': []}

    def command(argv, label, *, wall=10, limit=131072, check=True):
        evidence['stage'] = label
        result = diagnostic.supervise(argv, private / label, cwd=root, env=env,
                                       wall=wall, limit=limit)
        evidence['last_setup_exit_code'] = result['exit_code']
        if check and (result['reason'] != 'exit' or result['exit_code'] != 0):
            raise RuntimeError('bounded command failed')
        return result, (private / label / 'output.bin').read_text(errors='strict')

    try:
        install = Path(install_status).read_text().strip()
        if not install.isdecimal() or not 0 <= int(install) <= 255:
            raise RuntimeError('tool installation receipt')
        evidence['install_exit_code'] = int(install)
        if int(install) != 0:
            raise RuntimeError('tool installation incomplete')
        evidence['stage'] = 'source-binding'
        if Path(manifest_path).stat().st_size > 16384:
            raise RuntimeError('manifest budget')
        manifest = json.loads(Path(manifest_path).read_text())
        for key in ('github_sha', 'github_run_id', 'github_run_attempt'):
            if manifest[key] != os.environ['GITHUB_' + key[7:].upper()]:
                raise RuntimeError('manifest run binding')
        if not re.fullmatch('[0-9a-f]{40}', manifest['github_sha']):
            raise RuntimeError('source identity')
        if (manifest['source_base'] != diagnostic.BASE or
                manifest['source_hashes'] != diagnostic.SOURCE_HASHES or
                manifest['helper_sha256'] != APPROVED_HELPER or
                sha256(root / 'scripts/bootstrap_diagnostic.py') != APPROVED_HELPER):
            raise RuntimeError('approved source binding')
        _, head = command(['/usr/bin/git', 'rev-parse', 'HEAD'], 'head')
        if head.strip() != manifest['github_sha']:
            raise RuntimeError('checkout identity')
        protected = ['src', 'tests', 'Cargo.toml', 'Cargo.lock', 'build.rs']
        command(['/usr/bin/git', 'diff', '--quiet', diagnostic.BASE, '--', *protected], 'source-diff')
        _, extra = command(['/usr/bin/git', 'ls-files', '--others', '--', *protected], 'source-extra')
        if extra.strip():
            raise RuntimeError('untracked protected source')
        carry = root / 'target/debug/carry'
        if (manifest['carry'] != str(carry) or carry.resolve() != carry or
                not elf(carry) or sha256(carry) != manifest['carry_sha256']):
            raise RuntimeError('same emitted debug binary required')
        for name, expected in diagnostic.SOURCE_HASHES.items():
            if sha256(root / name) != expected:
                raise RuntimeError('original test source changed')
        evidence.update(source_sha=manifest['github_sha'], carry_sha256=sha256(carry),
                        manifest_sha256=sha256(manifest_path), helper_sha256=sha256(__file__))
        env = clean_environment(private, carry)
        _, packages = command([DPKG, '-W', '-f=${binary:Package}\t${Version}\t${db:Status-Status}\n',
                               'linux-tools-*'], 'packages')
        candidates = []
        installed = [line.split('\t') for line in packages.splitlines() if line.endswith('\tinstalled')]
        if len(installed) > 40:
            raise RuntimeError('package budget')
        for index, entry in enumerate(sorted(installed)):
            package, version, _ = entry
            if (not re.fullmatch(r'linux-tools-[a-zA-Z0-9+.:~-]{1,100}', package) or
                    not re.fullmatch(r'[a-zA-Z0-9.+:~_-]{1,128}', version)):
                raise RuntimeError('package metadata')
            _, listing = command([DPKG, '-L', package], 'package-' + str(index))
            for name in listing.splitlines():
                path = Path(name)
                if (path.name == 'perf' and path.is_absolute() and path.is_file() and
                        path.resolve() == path and os.access(path, os.X_OK) and
                        any(parent == path.parent or parent in path.parents for parent in TOOL_ROOTS)
                        and elf(path)):
                    candidates.append((package, version, str(path)))
        if not candidates:
            evidence['stage'] = 'package-owned-elf'
            raise RuntimeError('no package-owned perf ELF')
        package, version, perf = sorted(set(candidates))[0]
        _, owner = command([DPKG, '-S', perf], 'owner')
        if owner.strip() != package + ': ' + perf:
            raise RuntimeError('ELF owner mismatch')
        _, tool_version = command([perf, '--version'], 'version')
        match = re.fullmatch(r'perf version ([0-9][a-zA-Z0-9.+~_-]{0,127})\s*', tool_version)
        if not match:
            raise RuntimeError('tool version schema')
        evidence.update(perf_package=package, perf_package_version=version,
                        perf_version=match[1], perf_sha256=sha256(perf))
        _, timeout_version = command(['/usr/bin/timeout', '--version'], 'timeout-version')
        if not timeout_version.startswith('timeout (GNU coreutils) '):
            raise RuntimeError('GNU timeout required')
        # No sudo fallback, sysctl/capability changes or request retries. A runner
        # that denies unprivileged cpu-clock:u recording produces SETUP_BLOCKED.
        command(record_command(perf, private / 'permission.data', ['/usr/bin/true']),
                'permission', wall=35)
        _, names = command([NM, '--defined-only', '--no-demangle', '--just-symbols', str(carry)],
                           'public-symbols', limit=16777216)
        symbols = {name for name in names.splitlines() if 0 < len(name) <= 512 and name.isascii()
                   and all(32 < ord(char) < 127 for char in name)}
        if not symbols:
            raise RuntimeError('public ELF symbol table unavailable')
        evidence['status'] = 'PROFILE_BLOCKED'
        workload = [sys.executable, '-B', str(Path(__file__).resolve()), 'case',
                    '--root', str(root), '--private', str(private)]
        record, _ = command(record_command(perf, private / 'perf.data', workload),
                            'record', wall=35, check=False)
        evidence.update(record_exit_code=record['exit_code'],
                        record_supervisor_exit=int(record['reason'] == 'exit'),
                        record_elapsed_ns=record['elapsed_ns'])
        receipt = private / 'case-exit.json'
        if receipt.is_file():
            code = json.loads(receipt.read_text())['exit_code']
            if type(code) is not int or code not in (0, 1):
                raise RuntimeError('case receipt schema')
            evidence['case_exit_code'] = code
        case_result = private / 'trace/case-result.json'
        if case_result.is_file():
            case = json.loads(case_result.read_text())
            for key in ('tests', 'failures', 'errors', 'skips', 'dropped_events', 'response_write_errors'):
                if type(case[key]) is not int or not 0 <= case[key] <= 1000000:
                    raise RuntimeError('case result schema')
                evidence['case_' + key] = case[key]
        events = private / 'trace/events.jsonl'
        if events.is_file() and events.stat().st_size <= diagnostic.EVENT_LIMIT:
            evidence['case_socket_deadline_errors'] = sum(
                event.get('event') == 'client_getresponse_error' and event.get('error_type') == 'TimeoutError'
                for event in (json.loads(line) for line in events.read_text().splitlines()))
        data = private / 'perf.data'
        evidence['raw_data_bytes'] = data.stat().st_size if data.is_file() else 0
        _, text = command(['/usr/bin/timeout', '--kill-after=2s', '10s', perf, 'report',
                           '--stdio', '--stdio-color=never', '--no-children', '--no-demangle',
                           '--sort=comm,symbol', '--fields=overhead,sample,comm,symbol',
                           '--field-separator=\t', '--percentage=absolute', '--percent-limit=0',
                           '-i', str(data)], 'report', wall=15, limit=262144)
        comms = {'carry', 'tokio-runtime-w', Path(sys.executable).name[:15],
                 Path(sys.executable).resolve().name[:15]}
        evidence['lost_samples'] = report_loss(text)
        rows = symbol_rows(text, symbols, comms)
        known = sum(row['sample_count'] for row in rows if row['symbol'] in symbols)
        total = sum(row['sample_count'] for row in rows)
        evidence.update(rows=rows, sample_count=total, known_public_symbol_samples=known,
                        unknown_or_not_allowlisted_samples=total - known,
                        percentage_denominator='all-inherited-user-cpu-clock-sample-periods',
                        symbol_scope='same-emitted-carry-ELF-defined-mangled-symbols')
        # Availability threshold, NOT a statistical confidence/root-cause claim.
        enough = known >= 50 and 0 < evidence['raw_data_bytes'] < 16777216
        complete = (record['reason'] == 'exit' and record['exit_code'] == 0 and
                    evidence['case_exit_code'] == 0 and evidence.get('case_tests') == 1 and
                    all(evidence.get('case_' + key) == 0 for key in
                        ('failures', 'errors', 'skips', 'dropped_events', 'response_write_errors')))
        report_complete = evidence['lost_samples'] == 0 and report_samples(text) == total
        evidence['status'] = ('SAMPLES_AVAILABLE' if enough else 'INSUFFICIENT_SAMPLES') if complete else 'PROFILE_CASE_OR_RECORD_FAILED'
        if not report_complete:
            evidence['status'] = 'PROFILE_REPORT_INCOMPLETE'
        evidence['stage'] = 'complete'
        return 0 if complete and enough and report_complete else 1
    except Exception:
        evidence['internal_error'] = 1
        return 75 if evidence['status'] == 'SETUP_BLOCKED' else 1
    finally:
        diagnostic.dump(report, evidence)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['hosted', 'case'])
    parser.add_argument('--root', required=True, type=Path)
    parser.add_argument('--private', required=True, type=Path)
    parser.add_argument('--manifest', type=Path)
    parser.add_argument('--report', type=Path)
    parser.add_argument('--install-status', type=Path)
    args = parser.parse_args()
    if args.mode == 'case':
        return original_case(args.root, args.private)
    if not all((args.manifest, args.report, args.install_status)):
        parser.error('hosted requires manifest/report/install-status')
    return hosted(args.root, args.manifest, args.private, args.report, args.install_status)


if __name__ == '__main__':
    raise SystemExit(main())
