"""Definition-only proxy field preparation; native code supplies actual defaults."""
import copy
from typing import Any

from .fields import Field


def prepare_defaults(fields: dict[str, Field[Any]], behavior: dict[str, Any], model: str) -> None:
    for proxy in behavior.get("proxy_models", ()):
        if proxy["model"] != model:
            continue
        for name in proxy.get("defaults", ()):
            field = fields[name]
            if field.client_default:
                continue
            # A copy keeps a field shared with an earlier registry snapshot unchanged.
            replacement = copy.copy(field)
            replacement.client_default = True
            fields[name] = replacement
            setattr(field.model, name, replacement)
