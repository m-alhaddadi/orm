"""``pg_trgm``: trigram similarity; indexes that speed up ``LIKE`` / ``ILIKE`` / ``%``."""

from __future__ import annotations

from typing import Any, Literal

from ..schema import Extension, Index, Key, Sql

extension = Extension("pg_trgm")


def TrigramIndex(  # noqa: N802  (reads like a class at the call site)
    *fields: str | Sql,
    method: Literal["gin", "gist"] = "gin",
    **kwargs: Any,
) -> Index:
    """GIN (default) or GiST trigram index; makes ``contains()`` / ``icontains()``
    filters on these columns use an index."""
    opclass = f"{method}_trgm_ops"
    return Index(*(Key(f, opclass=opclass) for f in fields), method=method, **kwargs)
