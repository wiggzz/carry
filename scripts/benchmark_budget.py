"""Trusted controller escrow shared across arms/lanes; never an invoice.

An entire gateway lifetime receives the remaining model allowance. Until exact
container absence and a complete native journal are proved, none is refunded.
Cloud reservation is operator-supplied evidence, not an invented default.
"""
from contextlib import contextmanager
from decimal import Decimal, ROUND_CEILING
import fcntl
import hashlib
import json
import os
from pathlib import Path
import uuid

try:
    from proxy_benchmark import native_cost_decimal
except ModuleNotFoundError:
    from scripts.proxy_benchmark import native_cost_decimal

SCALE = Decimal(10) ** 12
CATALOG = Path(__file__).resolve().parents[1] / 'src/model_catalog.json'


def pico(value):
    amount = Decimal(str(value))
    if not amount.is_finite() or amount < 0:
        raise ValueError('invalid spending amount')
    return int((amount * SCALE).to_integral_value(rounding=ROUND_CEILING))


class SpendingAuthority:
    def __init__(self, path):
        self.path = Path(path).resolve()

    @staticmethod
    def create(path, *, cap_usd, cloud_reserve_usd, cloud_bound):
        cap, cloud = pico(cap_usd), pico(cloud_reserve_usd)
        if not 0 <= cloud < cap:
            raise ValueError('cloud reservation leaves no model allowance')
        if (not isinstance(cloud_bound, dict) or not cloud_bound.get('cleanup_owner') or
                type(cloud_bound.get('maximum_seconds')) is not int or cloud_bound['maximum_seconds'] <= 0):
            raise ValueError('finite cloud lifetime and cleanup evidence required')
        bound = (Decimal(str(cloud_bound['rate_usd_per_hour'])) *
                 cloud_bound['maximum_seconds'] / Decimal(3600))
        if pico(bound) > cloud:
            raise ValueError('cloud reservation does not cover declared envelope')
        state = {'schema_version': 1, 'cap_pico_usd': str(cap),
                 'cloud_reserved_pico_usd': str(cloud), 'cloud_bound': cloud_bound,
                 'catalog_sha256': hashlib.sha256(CATALOG.read_bytes()).hexdigest(),
                 'available_pico_usd': str(cap-cloud), 'settled_pico_usd': '0',
                 'active': None, 'history': []}
        with Path(path).open('x') as stream:
            os.chmod(path, 0o600)
            json.dump(state, stream, sort_keys=True); stream.flush(); os.fsync(stream.fileno())

    @contextmanager
    def _transaction(self):
        with self.path.with_suffix(self.path.suffix + '.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            state = json.loads(self.path.read_text()) # Missing/corrupt authority never starts fresh.
            if (state['schema_version'] != 1 or state['catalog_sha256'] !=
                    hashlib.sha256(CATALOG.read_bytes()).hexdigest()):
                raise ValueError('shared spending authority source mismatch')
            yield state
            temporary = self.path.with_name(self.path.name + '.' + uuid.uuid4().hex)
            try:
                with temporary.open('x') as stream:
                    os.chmod(temporary, 0o600)
                    json.dump(state, stream, sort_keys=True); stream.flush(); os.fsync(stream.fileno())
                os.replace(temporary, self.path)
                fd = os.open(self.path.parent, os.O_DIRECTORY)
                try:
                    os.fsync(fd)
                finally:
                    os.close(fd)
            finally:
                temporary.unlink(missing_ok=True)

    def validate(self, *, cap_usd=None, measured=False):
        with self._transaction() as state:
            if cap_usd is not None and str(pico(cap_usd)) != state['cap_pico_usd']:
                raise ValueError('shared spending authority cap mismatch')
            if measured and state['cloud_bound'].get('evidence_kind') != 'measured-bound':
                raise ValueError('measured cloud setup/cleanup bound required')
            if state['active']:
                raise ValueError('shared spending authority has outstanding exposure')
        return self

    def reserve(self, identity):
        with self._transaction() as state:
            if state['active']:
                raise ValueError('shared spending authority has outstanding exposure')
            if int(state['available_pico_usd']) <= 0:
                raise ValueError('shared spending authority exhausted')
            lease = {'id': uuid.uuid4().hex, 'identity': identity,
                     'pico_usd': state['available_pico_usd']}
            state['active'] = lease
            state['available_pico_usd'] = '0'
        return lease

    def settle(self, lease, journal):
        # Called only after the existing exact-resource cleanup has succeeded.
        # No journal, crash, ambiguity or unknown tariff can refund the escrow.
        events = [json.loads(line) for line in Path(journal).read_text().splitlines()]
        if not events or events[0] != {'event': 'escrow', 'pico_usd': lease['pico_usd'], 'lease_id': lease['id']}:
            raise ValueError('spending journal escrow mismatch')
        reservations, terminals = {}, {}
        for event in events[1:]:
            request = event['request_id']
            if event['event'] == 'reserved':
                if request in reservations or request in terminals:
                    raise ValueError('duplicate spending reservation')
                reservations[request] = event
            elif event['event'] == 'completed' and request in reservations and request not in terminals:
                terminals[request] = event
            else:
                raise ValueError('unknown or incomplete paid attempt')
        if reservations.keys() != terminals.keys():
            raise ValueError('incomplete spending journal')
        if sum(int(e['pico_usd']) for e in reservations.values()) > int(lease['pico_usd']):
            raise ValueError('spending reservations exceed escrow')
        cost = 0
        for request, terminal in terminals.items():
            if terminal['model'] != reservations[request]['model']:
                raise ValueError('paid model mismatch')
            native = native_cost_decimal(terminal['model'], terminal['usage'],
                                         service_tier=terminal.get('service_tier'))
            if native is None:
                raise ValueError('unknown paid usage')
            charged = pico(native)
            if charged > int(reservations[request]['pico_usd']):
                raise ValueError('native charge exceeds reserved exposure')
            cost += charged
        with self._transaction() as state:
            if state['active'] != lease:
                raise ValueError('spending lease mismatch')
            state['available_pico_usd'] = str(int(lease['pico_usd']) - cost)
            state['settled_pico_usd'] = str(int(state['settled_pico_usd']) + cost)
            state['history'].append({**lease, 'native_pico_usd': str(cost),
                                    'journal_sha256': hashlib.sha256(Path(journal).read_bytes()).hexdigest()})
            state['active'] = None
