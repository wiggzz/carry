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
        return [Path("/pier/base.yaml"), Path("/pier/generated-mounts.yaml")]


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


class RuntaDockerEnvironmentTests(unittest.TestCase):
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
