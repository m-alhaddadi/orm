"""Model base class and the model registry."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING, Any, ClassVar

from . import _native
from .errors import DoesNotExist, MultipleObjectsReturned
from .expr import ColumnRef
from .fields import MISSING, BelongsTo, Field, Relation

if TYPE_CHECKING:
    from typing_extensions import Self

    from .db import Database
    from .query import QuerySet

__all__ = ["Model", "ModelMeta", "Registry", "registry"]


class ModelMeta:
    """Schema information about one model (``User._meta``)."""

    def __init__(self, model: type[Model], table: str) -> None:
        self.model = model
        self.name = model.__name__
        self.table = table
        self.fields: dict[str, Field[Any]] = {}
        self.relations: dict[str, Relation[Any, Any]] = {}
        for klass in reversed(model.__mro__):
            for attr, value in vars(klass).items():
                if isinstance(value, Field):
                    self.fields[attr] = value
                elif isinstance(value, Relation):
                    self.relations[attr] = value
        pks = [f for f in self.fields.values() if f.primary_key]
        if len(pks) != 1:
            raise TypeError(f"model {self.name} must declare exactly one primary key")
        self.pk: Field[Any] = pks[0]
        # Row tuples from the engine follow this order.
        self.field_names: tuple[str, ...] = tuple(self.fields)

    def pk_ref(self) -> ColumnRef[Any]:
        return ColumnRef(self.model, (), self.pk)

    def ir(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "table": self.table,
            "fields": [f.ir() for f in self.fields.values()],
            "relations": [r.ir() for r in self.relations.values()],
        }

    def __repr__(self) -> str:
        return f"<ModelMeta {self.name} table={self.table!r}>"


class Registry:
    """All model classes known to the process; compiled once into the native schema."""

    def __init__(self) -> None:
        self._models: dict[str, type[Model]] = {}
        self._native: _native.Schema | None = None

    def register(self, model: type[Model]) -> None:
        name = model.__name__
        if name in self._models and self._models[name] is not model:
            raise TypeError(f"a model named {name} is already registered")
        self._models[name] = model
        self._native = None

    def get(self, name: str) -> type[Model]:
        try:
            return self._models[name]
        except KeyError:
            raise LookupError(f"unknown model {name!r}; is its module imported?") from None

    def __iter__(self) -> Any:
        return iter(self._models.values())

    def ir(self) -> dict[str, Any]:
        return {"models": [m._meta.ir() for m in self._models.values()]}

    def native(self) -> _native.Schema:
        if self._native is None:
            self._native = _native.Schema(json.dumps(self.ir()))
        return self._native


registry = Registry()


class Model:
    """Base class of generated model classes.

    Instances keep column values in their ``__dict__``. ``Model.objects`` is the root
    :class:`~orm.query.QuerySet` of the model.
    """

    _meta: ClassVar[ModelMeta]
    objects: ClassVar[QuerySet[Any]]
    DoesNotExist: ClassVar[type[DoesNotExist]] = DoesNotExist
    MultipleObjectsReturned: ClassVar[type[MultipleObjectsReturned]] = MultipleObjectsReturned

    # Instances built from database rows skip __init__ and so read this class default.
    _persisted: bool = True

    def __init_subclass__(cls, *, table: str | None = None, **kwargs: Any) -> None:
        super().__init_subclass__(**kwargs)
        from .query import QuerySet

        cls._meta = ModelMeta(cls, table or cls.__name__.lower())
        cls.objects = QuerySet(cls)
        for base in (DoesNotExist, MultipleObjectsReturned):
            ns = {"__qualname__": f"{cls.__qualname__}.{base.__name__}", "__module__": cls.__module__}
            setattr(cls, base.__name__, type(base.__name__, (base,), ns))
        registry.register(cls)

    def __init__(self, **values: Any) -> None:
        meta = self._meta
        d = self.__dict__
        for name, field in meta.fields.items():
            if name in values:
                d[name] = values.pop(name)
            elif field.default is not MISSING:
                d[name] = field.python_default()
            elif field.nullable:
                d[name] = None
        for name, value in values.items():
            rel = meta.relations.get(name)
            if not isinstance(rel, BelongsTo):
                raise TypeError(f"{meta.name}() got an unexpected keyword argument {name!r}")
            setattr(self, name, value)
        d["_persisted"] = False

    @classmethod
    def _from_row(cls, row: tuple[Any, ...]) -> Self:
        obj = cls.__new__(cls)
        obj.__dict__.update(zip(cls._meta.field_names, row))
        return obj

    def __setattr__(self, name: str, value: Any) -> None:
        # Track assignments so save() only writes what changed. Reads stay plain
        # __dict__ lookups; rows from the database bypass this entirely.
        object.__setattr__(self, name, value)
        meta = self._meta
        if name in meta.fields:
            self.__dict__.setdefault("_dirty", set()).add(name)
        elif isinstance(rel := meta.relations.get(name), BelongsTo):
            self.__dict__.setdefault("_dirty", set()).add(rel.via)

    @property
    def pk(self) -> Any:
        return self.__dict__.get(self._meta.pk.name)

    # -- persistence ---------------------------------------------------------------------

    def _sync_foreign_keys(self) -> None:
        """Pick up keys of related objects that were unsaved when assigned."""
        d = self.__dict__
        for rel in self._meta.relations.values():
            if isinstance(rel, BelongsTo) and d.get(rel.via) is None and d.get(rel.name) is not None:
                d[rel.via] = getattr(d[rel.name], rel.to)
                d.setdefault("_dirty", set()).add(rel.via)

    def _insert_row(self) -> list[Any]:
        self._sync_foreign_keys()
        d = self.__dict__
        row = []
        for name, field in self._meta.fields.items():
            if name in d:
                row.append(d[name])
            elif field.has_server_value:
                row.append(_native.DEFAULT)
            else:
                raise ValueError(f"{self._meta.name}.{name} is required")
        return row

    def _apply_row(self, row: tuple[Any, ...]) -> None:
        d = self.__dict__
        d.update(zip(self._meta.field_names, row))
        d["_persisted"] = True
        d.pop("_dirty", None)

    async def save(self, *, using: Database | None = None) -> None:
        """INSERT a new instance (filling in database defaults), or UPDATE the fields
        assigned since it was loaded or last saved."""
        from .db import resolve

        db = resolve(using)
        meta = self._meta
        if not self._persisted:
            rows = await db._insert(meta.name, list(meta.field_names), [self._insert_row()])
            self._apply_row(rows[0])
            return
        self._sync_foreign_keys()
        d = self.__dict__
        dirty = d.get("_dirty", ())
        values = {n: d[n] for n in meta.field_names if n in dirty and n != meta.pk.name}
        if values:
            await type(self).objects.using(db).filter(meta.pk_ref() == self.pk).update(**values)
        d.pop("_dirty", None)

    async def delete(self, *, using: Database | None = None) -> None:
        await type(self).objects.using(using).filter(self._meta.pk_ref() == self.pk).delete()
        self.__dict__["_persisted"] = False

    async def refresh(self, *, using: Database | None = None) -> None:
        """Reload column values from the database."""
        fresh = await type(self).objects.using(using).get(self._meta.pk_ref() == self.pk)
        self._apply_row(tuple(fresh.__dict__[n] for n in self._meta.field_names))

    # -- dunder --------------------------------------------------------------------------

    def __eq__(self, other: object) -> bool:
        if type(other) is not type(self):
            return NotImplemented
        pk = self.pk
        return pk is not None and pk == other.pk

    def __hash__(self) -> int:
        if self.pk is None:
            raise TypeError("model instances without a primary key value are unhashable")
        return hash((type(self), self.pk))

    def __repr__(self) -> str:
        d = self.__dict__
        shown = ", ".join(f"{n}={d[n]!r}" for n in self._meta.field_names if n in d)
        return f"{type(self).__name__}({shown})"

