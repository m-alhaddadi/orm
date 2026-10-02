"""``citext``: case-insensitive text. Values are plain ``str``."""

from __future__ import annotations

from typing import Any, TypeVar

from ..fields import Field
from ..schema import Extension

T = TypeVar("T")

extension = Extension("citext")


class CIText(Field[T]):
    """``citext`` column: comparisons, ``unique`` and ``on_conflict`` ignore case.

    Bound values are cast to ``citext`` so ``CIText == "A@X.IO"`` compares
    case-insensitively (a plain ``text`` parameter would pick the case-sensitive
    operator).
    """

    type_name = "text"
    db_type = "citext"
    write_sql = "CAST({} AS citext)"
    requires = ("citext",)

    def __init__(self, **kwargs: Any) -> None:
        super().__init__(**kwargs)
