"""Pier Docker environment that appends the managed-runtime CA overlay."""

from __future__ import annotations

from pathlib import Path

from pier.environments.docker.docker import DockerEnvironment


class RuntaDockerEnvironment(DockerEnvironment):
    """Preserve Pier's generated mounts while applying the standard CA overlay."""

    def __init__(self, *, runta_compose_file: str, **kwargs: object) -> None:
        super().__init__(**kwargs)
        self._runta_compose_file = Path(runta_compose_file)

    @property
    def _docker_compose_paths(self) -> list[Path]:
        return [*super()._docker_compose_paths, self._runta_compose_file]
