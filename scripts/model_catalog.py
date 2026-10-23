"""Read the same immutable model records embedded in the Carry executable."""
import json
from pathlib import Path

_CATALOG = json.loads((Path(__file__).resolve().parents[1] / 'src/model_catalog.json').read_text())


def model_spec(model):
    """Exact IDs only; an unknown model has no invented price or context limit."""
    return _CATALOG.get(model)
