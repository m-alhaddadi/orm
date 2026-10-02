"""``btree_gist``: lets scalar columns join GiST indexes and exclusion constraints."""

from __future__ import annotations

from typing import Any

from ..schema import Exclude, Extension, Key, Sql

extension = Extension("btree_gist")


def NoOverlap(  # noqa: N802
    *equal: str, start: str, end: str, range_type: str = "tstzrange", **kwargs: Any
) -> Exclude:
    """No two rows with the same ``equal`` columns have overlapping
    ``[start, end)`` ranges, e.g. room bookings::

        NoOverlap("room_id", start="starts_at", end="ends_at")
    """
    elements: list[tuple[str | Sql | Key, str]] = [(f, "=") for f in equal]
    elements.append((Sql(f'{range_type}("{start}", "{end}")'), "&&"))
    return Exclude(*elements, requires=["btree_gist"], **kwargs)
