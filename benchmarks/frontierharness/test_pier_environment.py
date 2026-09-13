#!/usr/bin/env python3
"""Exercise the Pier environment adapter without Docker or a live runtime."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import types
import unittest


ROOT = Path(__file__).parent
TARGET = ROOT / "carry_frontierharness" / "pier_environment.py"


class _DockerEnvironment:
    def __init__(self, **kwargs: object) -> None:
        self.received = kwargs

    @property
    def _docker_compose_paths(self) -> list[Path]:
        paths = [Path("/pier/base.yaml")]
        config = self.received.get("task_env_config")
        if config is not None and not bool(getattr(config, "allow_internet", True)):
            paths.append(Path("/pier/no-network.yaml"))
        paths.append(Path("/pier/generated-mounts.yaml"))
        return paths


def _install_pier_stubs() -> None:
    pier = types.ModuleType("pier")
    environments = types.ModuleType("pier.environments")
    docker = types.ModuleType("pier.environments.docker")
    docker.__path__ = []
    docker_impl = types.ModuleType("pier.environments.docker.docker")
    docker_impl.DockerEnvironment = _DockerEnvironment
    sys.modules.update(
        {
            "pier": pier,
            "pier.environments": environments,
            "pier.environments.docker": docker,
            "pier.environments.docker.docker": docker_impl,
        }
    )


class _NoNetworkTaskConfig:
    def __init__(self, allow_internet: bool) -> None:
        self.allow_internet = allow_internet

    def model_copy(self, *, update: dict[str, object]) -> "_NoNetworkTaskConfig":
        return _NoNetworkTaskConfig(bool(update.get("allow_internet", self.allow_internet)))


class RuntaDockerEnvironmentTests(unittest.TestCase):
    def test_enables_only_the_agent_container_model_egress(self) -> None:
        _install_pier_stubs()
        spec = importlib.util.spec_from_file_location("pier_environment_under_test", TARGET)
        assert spec and spec.loader
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        original = _NoNetworkTaskConfig(allow_internet=False)
        environment = module.RuntaDockerEnvironment(
            runta_compose_file="/work/runta-ca-overlay.yaml", task_env_config=original
        )
        self.assertFalse(original.allow_internet)
        self.assertTrue(environment.received["task_env_config"].allow_internet)
        self.assertNotIn(Path("/pier/no-network.yaml"), environment._docker_compose_paths)

    def test_appends_runta_overlay_after_pier_generated_mounts(self) -> None:
        _install_pier_stubs()
        spec = importlib.util.spec_from_file_location("pier_environment_under_test", TARGET)
        assert spec and spec.loader
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        environment = module.RuntaDockerEnvironment(
            runta_compose_file="/work/runta-ca-overlay.yaml", ordinary_option="preserved"
        )
        self.assertEqual(
            environment._docker_compose_paths,
            [
                Path("/pier/base.yaml"),
                Path("/pier/generated-mounts.yaml"),
                Path("/work/runta-ca-overlay.yaml"),
            ],
        )
        self.assertEqual(environment.received, {"ordinary_option": "preserved"})


if __name__ == "__main__":
    unittest.main()
