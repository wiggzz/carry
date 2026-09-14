"""Pier Docker environment that appends the managed-runtime CA overlay."""

from __future__ import annotations

from pathlib import Path

from pier.environments.docker.docker import DockerEnvironment


class RuntaDockerEnvironment(DockerEnvironment):
    """Use controlled runtime egress for Carry while preserving task verifier isolation.

    DeepSWE marks agent containers ``no-network``. That is correct for task
    code, but Carry itself runs in that same container and needs its first-hop
    model request. The Runta runtime independently enforces its frozen allowlist
    and injects provider authorization, so clone only the agent environment
    config with Docker networking enabled; the parsed verifier config remains
    untouched and continues to run no-network.
    """

    def __init__(self, *, runta_compose_file: str, **kwargs: object) -> None:
        task_env_config = kwargs.get("task_env_config")
        if task_env_config is not None:
            model_copy = getattr(task_env_config, "model_copy", None)
            if not callable(model_copy):
                raise TypeError("Pier task environment must support model_copy")
            kwargs["task_env_config"] = model_copy(update={"allow_internet": True})
        super().__init__(**kwargs)
        self._runta_compose_file = Path(runta_compose_file)

    @property
    def _docker_compose_paths(self) -> list[Path]:
        return [*super()._docker_compose_paths, self._runta_compose_file]
