"""Shared model facts and explicit native-client policy, never role defaults."""
import json
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[1]
_PATH = _ROOT / 'src/model_catalog.json'
if not _PATH.is_file():  # Packaged beside the harness's bin/ directory.
    _PATH = _ROOT / 'model_catalog.json'
_CATALOG = json.loads(_PATH.read_text())


def model_spec(model):
    """Exact IDs only; an unknown model has no invented price or context limit."""
    return _CATALOG.get(model)


def pi_model(model, *, name):
    """Project facts plus a reviewed client output allowance into Pi's schema.

    maxTokens is a configured client allowance, not an assertion that an unknown
    provider maximum is known. Keep that distinction in the source catalog.
    """
    spec = model_spec(model)
    if spec is None:
        raise ValueError('Pi model requires an exact shared catalog entry')
    context = spec.get('context_tokens')
    allowance = spec.get('client_output_tokens')
    if any(type(value) is not int or value <= 0 for value in (context, allowance)):
        raise ValueError('Pi model requires catalog context and explicit client output policy')
    maximum = spec.get('max_output_tokens')
    if allowance > context or (maximum is not None and allowance > maximum):
        raise ValueError('Pi client output policy exceeds a known model bound')
    rates = spec['rates']
    return {'id': model, 'name': name, 'reasoning': True, 'input': ['text', 'image'],
            'contextWindow': context, 'maxTokens': allowance,
            'cost': {'input': rates['input'], 'output': rates['output'],
                     'cacheRead': rates['cached'], 'cacheWrite': rates['write']}}
