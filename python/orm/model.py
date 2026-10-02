"""Model base class and the model registry."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING, Any, ClassVar

from . import _native
from .errors import DoesNotExist, MultipleObjectsReturned
from .expr import ColumnRef
from .fields import Field, Relation

if TYPE_CHECKING:
    from typing_extensions import Self

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

    def __init_subclass__(cls, *, table: str | None = None, **kwargs: Any) -> None:
        super().__init_subclass__(**kwargs)
        from .query import QuerySet

        cls._meta = ModelMeta(cls, table or cls.__name__.lower())
        cls.objects = QuerySet(cls)
        for base in (DoesNotExist, MultipleObjectsReturned):
            ns = {"__qualname__": f"{cls.__qualname__}.{base.__name__}", "__module__": cls.__module__}
            setattr(cls, base.__name__, type(base.__name__, (base,), ns))
        registry.register(cls)

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        name = type(self).__name__
        raise TypeError(
            f"{name} instances come from the database; insert a row with "
            f"`await {name}.objects.insert(...)`"
        )

    @classmethod
    def _from_row(cls, row: tuple[Any, ...]) -> Self:
        obj = cls.__new__(cls)
        obj.__dict__.update(zip(cls._meta.field_names, row))
        return obj

    def __setattr__(self, name: str, value: Any) -> None:
        raise AttributeError(
            f"{type(self).__name__} instances are read-only snapshots of a row; "
            f"write with `await obj.update({name}=...)`"
        )

    def __delattr__(self, name: str) -> None:
        raise AttributeError(f"{type(self).__name__} instances are read-only")

    @property
    def pk(self) -> Any:
        return self.__dict__.get(self._meta.pk.name)

    # -- writes --------------------------------------------------------------------------
    # Each method is one statement on this row (matched by primary key), run on the
    # database the instance was read from.

    def _row_query(self) -> QuerySet[Self]:
        return type(self).objects.using(self.__dict__.get("_db")).filter(self._meta.pk_ref() == self.pk)

    def _apply_row(self, row: tuple[Any, ...]) -> None:
        self.__dict__.update(zip(self._meta.field_names, row))

    async def update(self, **values: Any) -> None:
        """``UPDATE ... SET <values> WHERE pk = ... RETURNING *``.

        Values may be expressions (``views=Post.views + 1``); the instance is refreshed
        from the returned row, so it shows what the database stored.
        """
        if not values:
            return
        rows = await self._row_query()._update(values, returning=True)
        if not rows:
            raise self.DoesNotExist(f"{type(self).__name__} {self.pk!r} no longer exists")
        self._apply_row(rows[0])

    async def delete(self) -> None:
        """``DELETE ... WHERE pk = ...``. The instance keeps its last values."""
        await self._row_query().delete()

    async def refresh(self) -> None:
        """Reload column values from the database."""
        fresh = await self._row_query().get()
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

