Implement layered JSON configuration loading across src/config_merge.py and src/config_loader.py.

deep_merge(base, override) must recursively merge dictionaries. An override value that is not a dictionary replaces the base value (including lists). It must not mutate either input, including nested values.

load_config(path, environ=None) must:

- Read a JSON object from path.
- Support an optional top-level "include" containing either one path string or a list of path strings. Resolve includes relative to the file that declares them, recursively.
- Merge includes in listed order, then merge the including file, using deep_merge. Do not include the "include" key in the result.
- Expand every "${NAME}" occurrence in string values, including strings nested in lists and dictionaries. Use environ when provided, otherwise os.environ.
- Raise ValueError for invalid JSON, non-object documents, invalid include values, missing environment variables, and include cycles. A cycle error must show the filenames in the cycle.

Use only the Python standard library and do not change the public signatures. Run the test suite before finishing.
