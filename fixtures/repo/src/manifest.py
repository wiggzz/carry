"""Parsing for the small release-planning fixture."""

from dataclasses import dataclass
import re


_NAME = re.compile(r"^[A-Za-z][A-Za-z0-9_-]*$")


@dataclass(frozen=True)
class Package:
    name: str
    dependencies: tuple[str, ...]


def parse_manifest(text: str) -> list[Package]:
    """Parse name/dependency lines in declaration order."""
    # fixture:release-plan:start
    packages: list[Package] = []
    seen: set[str] = set()
    for line_number, raw_line in enumerate(text.splitlines(), 1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.count(":") != 1:
            raise ValueError(f"invalid manifest line {line_number}")
        raw_name, raw_dependencies = line.split(":", 1)
        name = raw_name.strip()
        if not _NAME.fullmatch(name):
            raise ValueError(f"invalid package name on line {line_number}: {name!r}")
        if name in seen:
            raise ValueError(f"duplicate package: {name}")

        dependencies: list[str] = []
        if raw_dependencies.strip():
            for raw_dependency in raw_dependencies.split(","):
                dependency = raw_dependency.strip()
                if not _NAME.fullmatch(dependency):
                    raise ValueError(
                        f"invalid dependency on line {line_number}: {dependency!r}"
                    )
                if dependency in dependencies:
                    raise ValueError(f"duplicate dependency for {name}: {dependency}")
                dependencies.append(dependency)

        packages.append(Package(name, tuple(dependencies)))
        seen.add(name)
    return packages
    # fixture:release-plan:end
