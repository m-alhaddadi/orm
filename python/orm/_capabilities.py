"""Immutable adapter selection for module/definition setup."""
from __future__ import annotations

import json
from . import _native

_metadata = json.loads(_native.profile_metadata())
_adapters = _metadata.get("adapters")
if (not isinstance(_adapters, list)
    or any(not isinstance(name, str) or _metadata["capabilities"].get(name) is not True for name in _adapters)):
    raise ImportError("native adapter/capability metadata mismatch")
ADAPTERS: frozenset[str] = frozenset(_adapters)
