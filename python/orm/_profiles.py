"""Select and validate one statically compiled engine during package initialization."""
from __future__ import annotations

import importlib
import importlib.util
import json
import os
from types import ModuleType

PROFILES = {
    "postgres": ("postgres",),
    "sqlite": ("sqlite",),
    "combined": ("postgres", "sqlite"),
    "tooling": ("postgres", "sqlite"),
    "custom": (),
}


def _build_record(build: object) -> bool:
    """The artifact's embedded Cargo features, rustc, target, profile and git revision."""
    if not isinstance(build, dict):
        return False
    features = build.get("features")
    return (isinstance(features, list) and all(isinstance(f, str) for f in features)
            and all(isinstance(build.get(key), str) for key in ("rustc", "target", "profile", "revision")))


def validate(module: ModuleType, profile: str) -> None:
    metadata = json.loads(module.profile_metadata())
    if not isinstance(metadata, dict):
        raise ImportError("incompatible orm native artifact; expected profile metadata object")
    if not _build_record(metadata.get("build")):
        raise ImportError("incompatible orm native artifact; rebuild it to embed its build record")
    if profile == "custom":
        backends = metadata.get("backends")
        capabilities = metadata.get("capabilities")
        adapters = metadata.get("adapters")
        if (metadata.get("abi") != 1 or metadata.get("version") != "0.1.0"
            or metadata.get("language") != "python" or metadata.get("profile") != "custom"
            or not isinstance(backends, list) or not backends
            or any(not isinstance(b, str) or b not in ("postgres", "sqlite") for b in backends)
            or len(set(backends)) != len(backends)
            or not isinstance(capabilities, dict) or any(type(v) is not bool for v in capabilities.values())
            or not isinstance(adapters, list) or any(not isinstance(a, str) or capabilities.get(a) is not True for a in adapters)
            or len(set(adapters)) != len(adapters)):
            raise ImportError("incompatible custom orm native artifact; rebuild for orm 0.1.0 ABI 1")
        return
    expected_capabilities = {
        "cli": profile == "tooling",
        "generate-python": profile == "tooling",
        "generate-typescript": profile == "tooling",
        "composition": False,
    }
    if (metadata.get("abi") != 1 or metadata.get("version") != "0.1.0"
        or metadata.get("language") != "python" or metadata.get("profile") != profile
        or tuple(metadata.get("backends", ())) != PROFILES[profile]
        or metadata.get("adapters") != []
        or metadata.get("capabilities") != expected_capabilities):
        raise ImportError(f"incompatible orm native profile {profile!r}; reinstall matching orm 0.1.0 packages")


def load() -> ModuleType:
    selected = os.environ.get("ORM_PROFILE")
    if selected is not None and selected not in PROFILES:
        raise ImportError(f"unknown ORM_PROFILE {selected!r}; choose {', '.join(PROFILES)}")
    available = [p for p in PROFILES if importlib.util.find_spec(f"orm_native_{p}") is not None]
    if selected is None:
        if len(available) != 1:
            raise ImportError(
                f"orm needs one native profile; installed: {', '.join(available) or 'none'}. "
                "Install orm[postgres], orm[sqlite], orm[combined] or orm[tooling]. "
                "Set ORM_PROFILE when multiple profiles are installed; extras do not combine native code."
            )
        selected = available[0]
    if selected not in available:
        raise ImportError(f"ORM_PROFILE={selected} is not installed; install orm[{selected}]")
    module = importlib.import_module(f"orm_native_{selected}._native")
    validate(module, selected)
    return module
