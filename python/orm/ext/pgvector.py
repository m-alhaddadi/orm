"""``vector`` (pgvector): embeddings and nearest-neighbour indexes.

Values are ``list[float]``. They travel as JSON arrays, which is also pgvector's text
format, and are cast to and from ``vector`` in SQL.
"""

from __future__ import annotations

from typing import Any, Literal, TypeVar

from ..fields import Field
from ..schema import Extension, Index, Key

T = TypeVar("T")

extension = Extension("vector")

Ops = Literal["vector_l2_ops", "vector_ip_ops", "vector_cosine_ops", "vector_l1_ops"]


class Vector(Field[T]):
    """``vector(dims)`` column."""

    type_name = "json"
    read_sql = "CAST(CAST({} AS text) AS jsonb)"
    requires = ("vector",)

    def __init__(self, dims: int, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.dims = dims
        self.db_type = f"vector({dims})"
        self.write_sql = f"CAST(CAST({{}} AS text) AS vector({dims}))"


def HnswIndex(  # noqa: N802
    field: str, *, ops: Ops = "vector_l2_ops", m: int | None = None, ef_construction: int | None = None, **kwargs: Any
) -> Index:
    params = {k: v for k, v in (("m", m), ("ef_construction", ef_construction)) if v is not None}
    return Index(Key(field, opclass=ops), method="hnsw", with_=params, **kwargs)


def IvfflatIndex(field: str, *, ops: Ops = "vector_l2_ops", lists: int = 100, **kwargs: Any) -> Index:  # noqa: N802
    return Index(Key(field, opclass=ops), method="ivfflat", with_={"lists": lists}, **kwargs)
