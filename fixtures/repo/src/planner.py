"""Dependency ordering for the small release-planning fixture."""

from .manifest import Package


def build_release_order(packages: list[Package]) -> list[str]:
    """Return a stable dependency-first order without mutating packages."""
    # fixture:release-plan:start
    by_name: dict[str, Package] = {}
    for package in packages:
        if package.name in by_name:
            raise ValueError(f"duplicate package: {package.name}")
        by_name[package.name] = package

    known = set(by_name)
    for package in packages:
        missing = [name for name in package.dependencies if name not in known]
        if missing:
            raise ValueError(
                f"unknown dependencies for {package.name}: {', '.join(missing)}"
            )

    remaining = {package.name: set(package.dependencies) for package in packages}
    order: list[str] = []
    while remaining:
        ready = [
            package.name
            for package in packages
            if package.name in remaining and not remaining[package.name]
        ]
        if not ready:
            involved = [
                package.name for package in packages if package.name in remaining
            ]
            raise ValueError(f"dependency cycle: {', '.join(involved)}")
        for name in ready:
            order.append(name)
            del remaining[name]
        for dependencies in remaining.values():
            dependencies.difference_update(ready)
    return order
    # fixture:release-plan:end
