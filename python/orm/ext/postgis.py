"""``postgis``: geometry and geography columns with GiST indexes.

Values are EWKT strings (``"SRID=4326;POINT(1 2)"``), converted with ``ST_AsEWKT`` /
``ST_GeomFromEWKT``.
"""

from __future__ import annotations

from typing import Any, TypeVar

from ..fields import Field
from ..schema import Extension, Index

T = TypeVar("T")

extension = Extension("postgis")


class Geometry(Field[T]):
    """``geometry(<shape>, <srid>)``, e.g. ``Geometry("Point", 4326)``."""

    type_name = "text"
    read_sql = "ST_AsEWKT({})"
    requires = ("postgis",)
    base = "geometry"

    def __init__(self, shape: str = "Geometry", srid: int = 4326, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.db_type = f"{self.base}({shape}, {srid})"
        self.write_sql = f"CAST(ST_GeomFromEWKT({{}}) AS {self.db_type})"


class Geography(Geometry[T]):
    """``geography(<shape>, <srid>)``: distances in metres on the spheroid."""

    base = "geography"


def SpatialIndex(field: str, **kwargs: Any) -> Index:  # noqa: N802
    return Index(field, method="gist", **kwargs)
