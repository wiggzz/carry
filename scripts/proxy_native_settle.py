"""Fixture-only native tool boundary: wait after primary for durable review settlement."""
import json
import hashlib
from pathlib import Path
import sys
import time


def wait_settled(directory, completed_requests, timeout=30):
    target = Path(directory)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        files = [target] if target.suffix == '.json' else list(target.glob('*.json'))
        if len(files) == 1:
            try:
                state = json.loads(files[0].read_text())
            except (OSError, ValueError):
                pass
            else:
                if (isinstance(state, dict)
                        and isinstance(state.get('completed_requests'), int)
                        and not isinstance(state.get('completed_requests'), bool)
                        and state['completed_requests'] >= completed_requests
                        and state.get('background_review_pending') is False):
                    return state
        time.sleep(.01)
    raise TimeoutError('native fixture primary/reviewer checkpoint did not settle')


def wait_lineage_settled(directory, tenant, session, branch, completed_requests, timeout=30):
    """Exact checkpoint FILE locator; never a retained-cut authority hash."""
    raw = json.dumps([tenant, session, branch], ensure_ascii=False, separators=(',', ':')).encode()
    path = Path(directory)/(hashlib.sha256(raw).hexdigest()+'.json')
    return wait_settled(path, completed_requests, timeout)


if __name__ == '__main__':
    wait_settled(sys.argv[1], int(sys.argv[2]))
