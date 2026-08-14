Implement the release planner across src/manifest.py and src/planner.py.

parse_manifest(text) must:

- Parse non-empty, non-comment lines in the form "name: dependency, dependency".
- Allow packages with no dependencies and ignore surrounding whitespace.
- Accept names that begin with an ASCII letter and then contain letters, digits, "_", or "-".
- Preserve declaration and dependency order in its returned Package values.
- Raise ValueError for malformed lines, invalid names, duplicate packages, or duplicate dependencies.

build_release_order(packages) must:

- Return every package name exactly once, with each dependency before its dependents.
- Be stable: whenever multiple packages are ready, use their original declaration order.
- Raise ValueError for duplicate package names, unknown dependencies, and dependency cycles. Cycle errors must name the packages involved.
- Not mutate the input list or its Package values.

Use only the Python standard library and do not change the public signatures. Run the test suite before finishing.
