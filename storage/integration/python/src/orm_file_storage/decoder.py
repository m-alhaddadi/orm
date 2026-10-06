"""Fixed shape decoder installed only by the selected binding materializer.

The host supplies public (logical field name, physical row slot) pairs. Hidden
helper slots must never be passed here. None remains loaded; omission remains
absent. Preparation performs no I/O and allocates no per-instance adapter state.
"""
from collections.abc import Callable, Mapping, Sequence
from typing import Any
from . import FileField


def prepare_decoder(fields: Mapping[str, FileField], public: Sequence[tuple[str, int]]) -> Callable[[Sequence[Any], int, dict[str, Any]], None]:
    slots = tuple((position, name, fields[name].decode) for name, position in public if name in fields)
    if any(position < 0 for position, _, _ in slots):
        raise ValueError("public file row slots must be nonnegative")

    def decode(values: Sequence[Any], base: int, destination: dict[str, Any]) -> None:
        for position, name, convert in slots:
            destination[name] = convert(values[base + position])

    return decode
