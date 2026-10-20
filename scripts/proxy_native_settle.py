"""Fixture-only native tool boundary: wait after primary for durable review settlement."""
import json
from pathlib import Path
import sys
import time


def wait_settled(directory, completed_requests, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        files = list(Path(directory).glob('*.json'))
        if len(files) == 1:
            try:
                state = json.loads(files[0].read_text())
            except (OSError, ValueError):
                pass
            else:
                if (state.get('completed_requests', 0) >= completed_requests
                        and state.get('background_review_pending') is False):
                    return
        time.sleep(.01)
    raise TimeoutError('native fixture primary/reviewer checkpoint did not settle')


if __name__ == '__main__':
    wait_settled(sys.argv[1], int(sys.argv[2]))
