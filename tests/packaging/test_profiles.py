"""Loader validation in isolation: avoid importing orm before selecting a profile."""
import importlib.util
import json
from pathlib import Path
from types import ModuleType
from unittest.mock import patch

import pytest

spec = importlib.util.spec_from_file_location("orm_profiles", Path(__file__).parents[2] / "python/orm/_profiles.py")
profiles = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profiles)


def artifact(name, **overrides):
    metadata = {"abi": 1, "version": "0.1.0", "language": "python", "profile": name,
                "backends": list(profiles.PROFILES[name]), "adapters": [],
                "capabilities": {"cli": name == "tooling", "generate-python": name == "tooling",
                                 "generate-typescript": name == "tooling", "composition": False}}
    metadata.update(overrides)
    module = ModuleType("fake_engine")
    module.profile_metadata = lambda: json.dumps(metadata)
    return module


@pytest.mark.parametrize("installed,selector,message", [
    ([], None, "installed: none"), (["postgres", "sqlite"], None, "extras do not combine"),
    (["sqlite"], "postgres", "is not installed"), (["sqlite"], "unknown", "unknown ORM_PROFILE"),
])
def test_bad_selection_fails_without_loading_engine(installed, selector, message):
    environment = {} if selector is None else {"ORM_PROFILE": selector}
    with patch.dict("os.environ", environment, clear=True), \
         patch.object(profiles.importlib.util, "find_spec", side_effect=lambda name: object() if name.removeprefix("orm_native_") in installed else None), \
         patch.object(profiles.importlib, "import_module") as load:
        with pytest.raises(ImportError, match=message):
            profiles.load()
        load.assert_not_called()


def test_selector_loads_exactly_one_profile():
    module = artifact("combined")
    with patch.dict("os.environ", {"ORM_PROFILE": "combined"}, clear=True), \
         patch.object(profiles.importlib.util, "find_spec", return_value=object()), \
         patch.object(profiles.importlib, "import_module", return_value=module) as load:
        assert profiles.load() is module
        load.assert_called_once_with("orm_native_combined._native")


@pytest.mark.parametrize("override", [{"abi": 2}, {"version": "0.2.0"}, {"language": "node"},
                                      {"backends": ["postgres", "sqlite"]}, {"capabilities": {"cli": True}}])
def test_incompatible_metadata(override):
    with pytest.raises(ImportError, match="incompatible"):
        profiles.validate(artifact("sqlite", **override), "sqlite")


def test_custom_profile_validates_actual_capabilities():
    module = artifact("sqlite", profile="custom", capabilities={"cli": False, "reference-loading": True}, adapters=["reference-loading"])
    profiles.validate(module, "custom")
    module.profile_metadata = lambda: json.dumps({"abi": 1, "version": "0.1.0", "language": "python", "profile": "custom",
        "backends": ["sqlite"], "capabilities": {"reference-loading": False}, "adapters": ["reference-loading"]})
    with pytest.raises(ImportError, match="incompatible custom"):
        profiles.validate(module, "custom")
