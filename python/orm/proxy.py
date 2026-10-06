"""Definition-only proxy field preparation; native code supplies actual defaults."""
from typing import Any

from .fields import Field


class _NativeDefault:
    __slots__ = ()
    @property
    def has_insert_default(self) -> bool:
        return True


_classes: dict[type[Field[Any]], type[Field[Any]]] = {}


def prepare_defaults(fields: dict[str, Field[Any]], defaults: dict[str, Any]) -> None:
    for name in defaults:
        field = fields[name]
        if isinstance(field, _NativeDefault):
            continue
        base = type(field)
        prepared = _classes.get(base)
        if prepared is None:
            prepared = type(f"NativeDefault{base.__name__}", (_NativeDefault, base), {"__slots__": ()})
            _classes[base] = prepared
        replacement: Field[Any] = prepared.__new__(prepared)
        replacement.__dict__.update(field.__dict__)
        fields[name] = replacement
        setattr(field.model, name, replacement)
