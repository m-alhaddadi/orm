"""Model base class, the model registry, and models built from a schema file."""

from __future__ import annotations

import copy
import enum
import json
import types
from os import PathLike
from typing import TYPE_CHECKING, Any, ClassVar, cast

from . import _native
from .errors import DoesNotExist, MultipleObjectsReturned
from .expr import ColumnRef
from .fields import BY_TYPE, Array, BelongsTo, Enum, Field, HasMany, HasOne, ManyToMany, Relation, String

if TYPE_CHECKING:
    from typing_extensions import Self

    from .query import QuerySet

__all__ = ["Model", "ModelMeta", "Registry", "registry", "define", "load", "loads"]


class ModelMeta:
    """Schema information about one model (``User._meta``)."""

    def __init__(self, model: type[Model], table: str, registry: Registry) -> None:
        self.model = model
        self.name = model.__name__
        self.table = table
        self.registry = registry
        # The model's schema IR when it was built from a compiled schema: it carries
        # everything (indexes, triggers, extension types) the Python side doesn't use.
        self.schema_ir: dict[str, Any] | None = None
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
        self.input_fields: dict[str, Field[Any]] = self.fields
        self.default_filter: dict[str, Any] | None = None

    def pk_ref(self) -> ColumnRef[Any]:
        return ColumnRef(self.model, (), self.pk)

    def ir(self) -> dict[str, Any]:
        if self.schema_ir is not None:
            return self.schema_ir
        return {
            "name": self.name,
            "table": self.table,
            "fields": [f.ir() for f in self.fields.values()],
            "relations": [r.ir() for r in self.relations.values()],
        }

    def __repr__(self) -> str:
        return f"<ModelMeta {self.name} table={self.table!r}>"


class Registry:
    """A set of models compiled together into one native schema.

    Models join the default :data:`registry` unless told otherwise (``define(...,
    registry=other)``), which is how one process can hold two versions of a schema,
    e.g. in migration tests.
    """

    def __init__(self, *, dialect: str | None = None) -> None:
        self._models: dict[str, type[Model]] = {}
        # Schema enums: their Python classes and IR, by name.
        self._enums: dict[str, type[enum.Enum]] = {}
        self._enum_ir: dict[str, dict[str, Any]] = {}
        # Schema-level IR (extensions, functions, extension catalog) from define().
        self._dialect: str | None = dialect
        self._schema_extra: dict[str, list[Any]] = {}
        self._behavior: dict[str, Any] = {}
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

    def get_enum(self, name: str) -> type[enum.Enum]:
        try:
            return self._enums[name]
        except KeyError:
            raise LookupError(f"unknown enum {name!r}; is its module imported?") from None

    def register_enum(self, ir: dict[str, Any], cls: type[enum.Enum]) -> None:
        name = ir["name"]
        if name in self._enum_ir and self._enum_ir[name] != ir:
            raise TypeError(f"an enum named {name} is already registered")
        self._enums[name] = cls
        self._enum_ir[name] = ir
        self._native = None

    def __iter__(self) -> Any:
        return iter(self._models.values())

    def ir(self) -> dict[str, Any]:
        out: dict[str, Any] = {"models": [m._meta.ir() for m in self._models.values()]}
        if self._enum_ir:
            out["enums"] = list(self._enum_ir.values())
        if self._dialect is not None:
            out["dialect"] = self._dialect
        out.update(self._schema_extra)
        if self._behavior:
            out["behavior"] = self._behavior
        return copy.deepcopy(out)

    def prepare(self) -> _native.Schema:
        """Prepare class declarations after registering a complete dependency batch."""
        if self._native is not None:
            return self._native
        snapshot = self._candidate()
        prepared = json.loads(_native.prepare_schema(json.dumps(snapshot.ir())))
        classes: dict[str, type] = {**snapshot._models, **snapshot._enums}
        associations: list[tuple[type[Model], dict[str, Any], dict[str, Field[Any]], dict[str, Relation[Any, Any]]]] = []
        for ir in prepared["models"]:
            if ir["name"] not in snapshot._models:
                raise TypeError("class preparation cannot add or rename models; use define() for transformed model identities")
            model = snapshot._models[ir["name"]]
            if model._meta.registry is not self:
                continue
            fields: dict[str, Field[Any]] = {}
            for f in ir["fields"]:
                name = f["name"]
                if name in _RESERVED:
                    raise TypeError(f"{ir['name']}.{name}: reserved model member")
                existing = model._meta.fields.get(name)
                if existing is None and hasattr(model, name):
                    raise TypeError(f"{ir['name']}.{name}: generated field collides with an existing model member")
                field = existing if existing is not None and existing.ir() == f else _field(f)
                if field is not existing:
                    original = existing or next((old for old in model._meta.fields.values() if old.column == f["column"]), None)
                    if original is not None and callable(original.default):
                        field.default = original.default
                    field.__set_name__(model, name)
                fields[name] = field
            for old in model._meta.fields.keys() - fields.keys():
                if old not in vars(model):
                    raise TypeError("class preparation cannot remove inherited members; use define() for this logical view")
            relations: dict[str, Relation[Any, Any]] = {}
            for r in ir.get("relations", ()):
                name = r["name"]
                existing_relation = model._meta.relations.get(name)
                if name in _RESERVED or (existing_relation is None and hasattr(model, name)):
                    raise TypeError(f"{ir['name']}.{name}: generated relation collides with an existing model member")
                relation = existing_relation if existing_relation is not None and existing_relation.ir() == r else _relation(r)
                if relation is not existing_relation:
                    relation.__set_name__(model, name)
                relations[name] = relation
            for old in model._meta.relations.keys() - relations.keys():
                if old not in vars(model):
                    raise TypeError("class preparation cannot remove inherited members; use define() for this logical view")
            associations.append((model, ir, fields, relations))
        methods_by_model: dict[str, dict[str, Any]] = {}
        for method in prepared.get("behavior", {}).get("methods", ()):
            model = snapshot._models[method["model"]]
            if model._meta.registry is not self:
                continue
            name = method["name"]
            if name in _RESERVED or hasattr(model, name):
                raise TypeError(f"{method['model']}.{name}: generated method collides with an existing model member")
            methods_by_model.setdefault(method["model"], {})[name] = getattr(_native, method["native_function"])
        native = _native.Schema(json.dumps(prepared), classes)
        snapshot._native = native
        snapshot._behavior = prepared.get("behavior", {})
        # Associations are published only after all native validation succeeds.
        for model, ir, fields, relations in associations:
            meta = model._meta
            for name in meta.fields.keys() - fields.keys():
                delattr(model, name)
            for name, field in fields.items():
                if meta.fields.get(name) is not field:
                    setattr(model, name, field)
            for name in meta.relations.keys() - relations.keys():
                delattr(model, name)
            for name, relation in relations.items():
                if meta.relations.get(name) is not relation:
                    setattr(model, name, relation)
            meta.relations = relations
            meta.fields = fields
            meta.table = ir["table"]
            meta.pk = next(field for field in fields.values() if field.primary_key)
            meta.schema_ir = ir
            meta.default_filter = next((d.get("filter") for d in prepared.get("behavior", {}).get("query_defaults", ()) if d["model"] == meta.name), None)
            meta.field_names = tuple(fields)
            computed = {f["field"] for f in snapshot._behavior.get("result_fields", ()) if f["model"] == meta.name}
            meta.input_fields = {k: v for k, v in fields.items() if k not in computed} if computed else fields
            for name, function in methods_by_model.get(meta.name, {}).items():
                setattr(model, name, staticmethod(function))
            meta.registry = snapshot
        self._native = native
        self._behavior = copy.deepcopy(snapshot._behavior)
        return native

    def _candidate(self) -> Registry:
        candidate = Registry()
        candidate._models = self._models.copy()
        candidate._enums = self._enums.copy()
        candidate._enum_ir = self._enum_ir.copy()
        candidate._dialect = self._dialect
        candidate._schema_extra = copy.deepcopy(self._schema_extra)
        candidate._behavior = copy.deepcopy(self._behavior)
        return candidate

    def native(self) -> _native.Schema:
        if self._native is None:
            raise TypeError("registry changed; call registry.prepare() after declaring the complete model batch")
        return self._native


_RESERVED = {"pk", "objects", "_meta", "DoesNotExist", "MultipleObjectsReturned", "update", "delete", "refresh", "to_dict", "_field_value", "_orm_internal"}

registry = Registry()


def _default_registry() -> Registry:
    return registry


def _field(ir: dict[str, Any]) -> Field[Any]:
    kwargs: dict[str, Any] = {
        "primary_key": ir.get("primary_key", False),
        "auto_increment": ir.get("auto_increment", False),
        "nullable": ir.get("nullable", False),
        "unique": ir.get("unique", False),
        "index": ir.get("index", False),
        "column": ir["column"],
        "default_now": ir.get("default_now", False),
        "server_default": "default_sql" in ir,
    }
    if "default" in ir:
        kwargs["default"] = ir["default"]
    if ir.get("array"):
        return Array(ir["type"], enum=ir.get("enum"), **kwargs)
    if "enum" in ir:
        return Enum(ir["enum"], stored=ir["type"], **kwargs)
    cls = BY_TYPE[ir["type"]]
    if cls is String:
        return String(ir.get("max_length"), **kwargs)
    return cls(**kwargs)


def _relation(ir: dict[str, Any]) -> Relation[Any, Any]:
    if ir["kind"] == "one" and ir.get("foreign_key"):
        return BelongsTo(ir["target"], via=ir["from"], to=ir["to"], on_delete=ir.get("on_delete", "no_action"))
    if ir["kind"] == "one":
        return HasOne(ir["target"], via=ir["to"], from_=ir["from"])
    if "through" in ir:
        t = ir["through"]
        return ManyToMany(
            ir["target"], through=t["model"], source=t["source"], target_field=t["target"], from_=ir["from"], to=ir["to"]
        )
    return HasMany(ir["target"], via=ir["to"], from_=ir["from"])


def _enum(ir: dict[str, Any], module: str | None) -> type[enum.Enum]:
    """The Python class of a schema enum: an ``IntEnum`` for int storage, otherwise a
    ``StrEnum`` whose values are the stored labels."""
    base: type[enum.Enum] = enum.IntEnum if ir.get("storage") == "int" else enum.StrEnum
    members = [(v["name"], v["value"]) for v in ir["values"]]
    cls: type[enum.Enum] = cast(Any, base)(ir["name"], members, module=module)
    return cls


def define(
    schema: str | dict[str, Any], *, registry: Registry | None = None, module: str | None = None
) -> dict[str, Any]:
    """Build model classes from a compiled schema (the JSON IR ``orm compile`` and
    :func:`load` produce). Generated ``models.py`` modules call this.

    Returns the model classes and the schema's enum classes, by name. They join
    ``registry`` (the default one unless given); ``module`` sets their ``__module__``.
    """
    ir: dict[str, Any] = json.loads(schema) if isinstance(schema, str) else json.loads(json.dumps(schema))
    destination = registry if registry is not None else _default_registry()
    context = json.dumps(destination.ir()) if destination._models else None
    ir = json.loads(_native.prepare_schema(json.dumps(ir), context))
    reg = destination._candidate()
    dialect = ir.get("dialect", "postgres")
    if reg._dialect is not None and reg._dialect != dialect:
        raise TypeError("schemas in one registry must target the same database")
    reg._dialect = dialect
    out: dict[str, Any] = {}
    for e in ir.get("enums", ()):
        cls_e = reg._enums.get(e["name"]) if reg._enum_ir.get(e["name"]) == e else None
        cls_e = cls_e or _enum(e, module)
        reg.register_enum(e, cls_e)
        out[e["name"]] = cls_e
    for m in ir["models"]:
        if m["name"] in destination._models:
            if m != destination._models[m["name"]]._meta.ir():
                raise TypeError(f"extension changed existing model {m['name']}; define dependent schemas together in a new registry")
            continue
        ns: dict[str, Any] = {f["name"]: _field(f) for f in m["fields"]}
        ns.update({r["name"]: _relation(r) for r in m.get("relations", ())})
        if module is not None:
            ns["__module__"] = module
        cls: type[Model] = types.new_class(
            m["name"], (Model,), {"table": m["table"], "registry": reg}, lambda body: body.update(ns)
        )
        for method in ir.get("behavior", {}).get("methods", ()):
            if method["model"] == m["name"]:
                if method["name"] in ns or method["name"] in _RESERVED:
                    raise TypeError(f"{m['name']}.{method['name']}: model method collision")
        cls._meta.schema_ir = m
        computed = {f["field"] for f in ir.get("behavior", {}).get("result_fields", ()) if f["model"] == m["name"]}
        if computed:
            cls._meta.input_fields = {k: v for k, v in cls._meta.fields.items() if k not in computed}
        out[m["name"]] = cls
    for key in ("extensions", "functions", "catalog"):
        items = reg._schema_extra.setdefault(key, [])
        items.extend(x for x in ir.get(key, ()) if x not in items)
        if not items:
            del reg._schema_extra[key]
    reg._behavior = copy.deepcopy(ir.get("behavior", {}))
    reg.prepare()
    destination._models = reg._models.copy()
    destination._enums = reg._enums.copy()
    destination._enum_ir = reg._enum_ir.copy()
    destination._dialect = reg._dialect
    destination._schema_extra = {k: v.copy() for k, v in reg._schema_extra.items()}
    destination._native = reg._native
    destination._behavior = copy.deepcopy(reg._behavior)
    return out


def load(
    path: str | PathLike[str], *, registry: Registry | None = None, module: str | None = None
) -> dict[str, Any]:
    """Compile a schema file (``schema.prisma``) and build its models: no generated code
    needed (generate ``models.py`` / ``.pyi`` for editor and type-checker support)."""
    return define(_native.compile_schema_file(str(path)), registry=registry, module=module)


class Model:
    """Base class of generated model classes.

    Instances keep column values in their ``__dict__``. ``Model.objects`` is the root
    :class:`~orm.query.QuerySet` of the model.
    """

    _meta: ClassVar[ModelMeta]
    objects: ClassVar[QuerySet[Any]]
    DoesNotExist: ClassVar[type[DoesNotExist]] = DoesNotExist
    MultipleObjectsReturned: ClassVar[type[MultipleObjectsReturned]] = MultipleObjectsReturned

    def __init_subclass__(cls, *, table: str | None = None, registry: Registry | None = None, **kwargs: Any) -> None:
        super().__init_subclass__(**kwargs)
        from .query import QuerySet

        reg = registry if registry is not None else _default_registry()
        cls._meta = ModelMeta(cls, table or cls.__name__.lower(), reg)
        cls.objects = QuerySet(cls)
        for base in (DoesNotExist, MultipleObjectsReturned):
            ns = {"__qualname__": f"{cls.__qualname__}.{base.__name__}", "__module__": cls.__module__}
            setattr(cls, base.__name__, type(base.__name__, (base,), ns))
        reg.register(cls)

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        name = type(self).__name__
        raise TypeError(
            f"{name} instances come from the database; insert a row with "
            f"`await {name}.objects.insert(...)`"
        )

    def __setattr__(self, name: str, value: Any) -> None:
        raise AttributeError(
            f"{type(self).__name__} instances are read-only snapshots of a row; "
            f"write with `await obj.update({name}=...)`"
        )

    def __delattr__(self, name: str) -> None:
        raise AttributeError(f"{type(self).__name__} instances are read-only")

    @property
    def pk(self) -> Any:
        return self.__dict__.get(self._meta.pk.name, self.__dict__.get("_orm_internal", {}).get(self._meta.pk.name))

    def _field_value(self, name: str) -> Any:
        """Internal key access for relation loaders; never marks a field loaded."""
        if name in self.__dict__:
            return self.__dict__[name]
        internal = self.__dict__.get("_orm_internal", {})
        if name in internal:
            return internal[name]
        from .errors import NotLoaded
        raise NotLoaded(f"{type(self).__name__}.{name} was not loaded")

    def to_dict(self) -> dict[str, Any]:
        """Serialize public loaded scalar fields, retaining loaded NULL values."""
        return {n: self.__dict__[n] for n in self._meta.field_names if n in self.__dict__}

    # -- writes --------------------------------------------------------------------------
    # Each method is one statement on this row (matched by primary key), run on the
    # database the instance was read from.

    def _row_query(self) -> QuerySet[Self]:
        return type(self).objects.without_defaults().using(self.__dict__.get("_db")).filter(self._meta.pk_ref() == self.pk)

    def _apply_row(self, row: tuple[Any, ...]) -> None:
        values = dict(zip(self._meta.field_names, row))
        for name, relation in self._meta.relations.items():
            source = relation.via if isinstance(relation, BelongsTo) else getattr(relation, "from_", None)
            if source and self._field_value(source) != values[source]:
                self.__dict__.pop(name, None)
        if "_orm_internal" in self.__dict__:
            loaded = set(self.__dict__) & self._meta.fields.keys()
            self.__dict__.update({n: values[n] for n in loaded})
            self.__dict__["_orm_internal"].update({n: values[n] for n in self.__dict__["_orm_internal"]})
        else:
            self.__dict__.update(values)

    async def update(self, **values: Any) -> None:
        """``UPDATE ... SET <values> WHERE pk = ... RETURNING *``.

        Values may be expressions (``views=Post.views + 1``); the instance is refreshed
        from the returned row, so it shows what the database stored.
        """
        if not values:
            return
        rows = await self._row_query().update(**values).returning()
        if not rows:
            raise self.DoesNotExist(f"{type(self).__name__} {self.pk!r} no longer exists")
        self._apply_row(tuple(rows[0].__dict__[n] for n in self._meta.field_names))

    async def delete(self) -> None:
        """``DELETE ... WHERE pk = ...``. The instance keeps its last values."""
        await self._row_query().delete()

    async def refresh(self, *fields: ColumnRef[Any]) -> None:
        """Reload column values from the database."""
        query = self._row_query()
        requested = query.only(*fields)._model_fields if fields else ()
        if "_orm_internal" in self.__dict__:
            names = [n for n in self._meta.field_names if n in self.__dict__]
            names.extend(n for n in requested or () if n not in names)
            query = query._clone(_model_fields=tuple(names))
        fresh = await query.get()
        for name, relation in self._meta.relations.items():
            source = relation.via if isinstance(relation, BelongsTo) else getattr(relation, "from_", None)
            if source and self._field_value(source) != fresh._field_value(source):
                self.__dict__.pop(name, None)
        for name in self._meta.field_names:
            self.__dict__.pop(name, None)
        self.__dict__.update({n: fresh.__dict__[n] for n in self._meta.field_names if n in fresh.__dict__})
        if "_orm_internal" in fresh.__dict__:
            self.__dict__["_orm_internal"] = fresh.__dict__["_orm_internal"]
        else:
            self.__dict__.pop("_orm_internal", None)

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



def loads(
    source: str, *, registry: Registry | None = None, module: str | None = None
) -> dict[str, Any]:
    """Like :func:`load`, for schema source text (``import`` paths resolve against the
    current directory)."""
    return define(_native.compile_schema(source), registry=registry, module=module)
