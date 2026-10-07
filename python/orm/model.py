"""Model base class, the model registry, and models built from a schema file."""

from __future__ import annotations

import copy
import enum
import json
import types
from os import PathLike
from typing import TYPE_CHECKING, Any, ClassVar, cast

from . import _native
from .errors import DoesNotExist, MultipleObjectsReturned, NotLoaded
from .expr import ColumnRef
from .fields import BY_TYPE, Array, BelongsTo, Enum, Field, HasMany, HasOne, ManyToMany, Relation, String

if TYPE_CHECKING:
    from typing_extensions import Self

    from .query import QuerySet

# The native artifact is fixed for the process; read its capabilities once.
_CAPABILITIES: tuple[str, ...] = tuple(json.loads(_native.native_artifact()).get("capabilities", ()))
# Only a composition artifact (schema transformations) can change an already prepared schema.
_REPREPARE: bool = ("schema-transformations" in _CAPABILITIES
                    or json.loads(_native.profile_metadata())["capabilities"].get("composition") is True)

_reference_adapter: types.ModuleType | None = None
if "reference-loading" in _CAPABILITIES:
    from . import _references
    _reference_adapter = _references

__all__ = ["Model", "ModelMeta", "Registry", "registry", "define", "load", "loads"]


_ABSENT = object()


def _detached(value: Any) -> Any:
    """A deep copy; empty containers and ``None`` skip the generic ``deepcopy`` walk."""
    if value is None:
        return None
    return copy.deepcopy(value) if value else type(value)()


def _peek(row: Model, name: str) -> Any:
    """A key value or ``_ABSENT``; an absent key never equals a loaded one."""
    data = row.__dict__
    return data[name] if name in data else data.get("_orm_internal", {}).get(name, _ABSENT)


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
        # Local fields that `attach` accepts; `None` for a model that is not a composed child.
        self.attach_fields: dict[str, Field[Any]] | None = None
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
        # The schema default order: `{"field", "desc"?, "nulls"?}` per column.
        self.default_order: list[dict[str, Any]] = []

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
        self._identities: dict[str, Any] | None = None
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
        if self._identities is not None:
            out["identities"] = self._identities
        if self._behavior:
            out["behavior"] = self._behavior
        return copy.deepcopy(out)

    def prepare(self) -> _native.Schema:
        """Prepare class declarations after registering a complete dependency batch."""
        return self._prepare(None)

    def _prepare(self, done: tuple[str, dict[str, Any]] | None) -> _native.Schema:
        """``done`` is the prepared JSON and IR these classes were just built from."""
        if self._native is not None:
            return self._native
        snapshot = self._candidate()
        if done is None:
            prepared_json = _native.prepare_schema(json.dumps(snapshot.ir()))
            prepared = json.loads(prepared_json)
        else:
            prepared_json, prepared = done
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
            if _reference_adapter is not None:
                _reference_adapter.validate(model, fields, relations, prepared.get("behavior", {}))
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
        native = _native.Schema(prepared_json, classes)
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
            policy: dict[str, Any] = next((d for d in prepared.get("behavior", {}).get("query_defaults", ()) if d["model"] == meta.name), {})
            meta.default_filter = policy.get("filter")
            meta.default_order = policy.get("order", [])
            meta.field_names = tuple(fields)
            pk_ir = next(f for f in ir["fields"] if f.get("primary_key"))
            meta.attach_fields = (
                {f["name"]: fields[f["name"]] for f in ir["fields"] if f.get("hints", {}).get("composition.local") == "true"}
                if pk_ir.get("hints", {}).get("composition.child") == "true"
                else None
            )
            computed = {f["field"] for f in snapshot._behavior.get("result_fields", ()) if f["model"] == meta.name}
            meta.input_fields = {k: v for k, v in fields.items() if k not in computed} if computed else fields
            for name, function in methods_by_model.get(meta.name, {}).items():
                setattr(model, name, staticmethod(function))
            meta.registry = snapshot
            if _reference_adapter is not None:
                _reference_adapter.install(model)
        self._native = native
        self._behavior = _detached(snapshot._behavior)
        return native

    def _candidate(self) -> Registry:
        candidate = Registry()
        candidate._models = self._models.copy()
        candidate._enums = self._enums.copy()
        candidate._enum_ir = self._enum_ir.copy()
        candidate._dialect = self._dialect
        candidate._schema_extra = _detached(self._schema_extra)
        candidate._behavior = _detached(self._behavior)
        candidate._identities = _detached(self._identities)
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
        "server_default": "default_sql" in ir or ir.get("hints", {}).get("composition.key-default") == "true",
    }
    if "default" in ir:
        kwargs["default"] = ir["default"]
    if "client_default" in ir:
        kwargs["client_default"] = ir["client_default"]
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
    schema: str | dict[str, Any], *, registry: Registry | None = None, module: str | None = None, required_capabilities: tuple[str, ...] = ()
) -> dict[str, Any]:
    """Build model classes from a compiled schema (the JSON IR ``orm compile`` and
    :func:`load` produce). Generated ``models.py`` modules call this.

    Returns the model classes and the schema's enum classes, by name. They join
    ``registry`` (the default one unless given); ``module`` sets their ``__module__``.
    """
    for capability in required_capabilities:
        if capability not in _CAPABILITIES:
            raise TypeError(f"generated models require {capability}; rebuild/select a compatible native artifact")
    destination = registry if registry is not None else _default_registry()
    context = json.dumps(destination.ir()) if destination._models else None
    if isinstance(schema, str):
        try:
            prepared_json = _native.prepare_schema(schema, context)
        except _native.SchemaError:
            json.loads(schema)  # malformed JSON keeps raising JSONDecodeError
            raise
    else:
        prepared_json = _native.prepare_schema(json.dumps(schema), context)
    ir: dict[str, Any] = json.loads(prepared_json)
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
        cls._meta.pk = cls._meta.fields[cls._meta.pk.name]
        computed = {f["field"] for f in ir.get("behavior", {}).get("result_fields", ()) if f["model"] == m["name"]}
        if computed:
            cls._meta.input_fields = {k: v for k, v in cls._meta.fields.items() if k not in computed}
        out[m["name"]] = cls
    for key in ("extensions", "functions", "catalog"):
        items = reg._schema_extra.setdefault(key, [])
        items.extend(x for x in ir.get(key, ()) if x not in items)
        if not items:
            del reg._schema_extra[key]
    reg._behavior = _detached(ir.get("behavior", {}))
    reg._identities = _detached(ir.get("identities"))
    reg._prepare(None if _REPREPARE else (prepared_json, ir))
    destination._models = reg._models.copy()
    destination._enums = reg._enums.copy()
    destination._enum_ir = reg._enum_ir.copy()
    destination._dialect = reg._dialect
    destination._schema_extra = {k: v.copy() for k, v in reg._schema_extra.items()}
    destination._native = reg._native
    destination._behavior = _detached(reg._behavior)
    destination._identities = _detached(reg._identities)
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
        raise NotLoaded(f"{type(self).__name__}.{name} was not loaded")

    def to_dict(self) -> dict[str, Any]:
        """Serialize public loaded scalar fields, retaining loaded NULL values."""
        return {n: self.__dict__[n] for n in self._meta.field_names if n in self.__dict__}

    # -- writes --------------------------------------------------------------------------
    # Each method is one statement on this row (matched by primary key), run on the
    # database the instance was read from.

    def _row_query(self) -> QuerySet[Self]:
        return type(self).objects.without_defaults().using(self.__dict__.get("_db")).filter(self._meta.pk_ref() == self.pk)

    def _loaded_query(self, *extra: str) -> QuerySet[Self]:
        """The row query; a partial instance keeps its public shape plus ``extra``."""
        query = self._row_query()
        if "_orm_internal" not in self.__dict__:
            return query
        names = [n for n in self._meta.field_names if n in self.__dict__]
        names.extend(n for n in extra if n not in names)
        return query._clone(_model_fields=tuple(names))

    def _replace_from(self, fresh: Model) -> None:
        for name, relation in self._meta.relations.items():
            source = relation.via if isinstance(relation, BelongsTo) else getattr(relation, "from_", None)
            if source and _peek(self, source) != _peek(fresh, source):
                self.__dict__.pop(name, None)
        for name in self._meta.field_names:
            self.__dict__.pop(name, None)
        self.__dict__.update({n: fresh.__dict__[n] for n in self._meta.field_names if n in fresh.__dict__})
        if "_orm_internal" in fresh.__dict__:
            self.__dict__["_orm_internal"] = fresh.__dict__["_orm_internal"]
        else:
            self.__dict__.pop("_orm_internal", None)

    async def update(self, **values: Any) -> None:
        """``UPDATE ... SET <values> WHERE pk = ... RETURNING`` the loaded fields.

        Values may be expressions (``views=Post.views + 1``); the instance is refreshed
        from the returned row, so it shows what the database stored.
        """
        if not values:
            return
        rows = await self._loaded_query().update(**values).returning()
        if not rows:
            raise self.DoesNotExist(f"{type(self).__name__} {self.pk!r} no longer exists")
        self._replace_from(rows[0])

    async def delete(self) -> None:
        """``DELETE ... WHERE pk = ...``. The instance keeps its last values."""
        await self._row_query().delete()

    async def refresh(self, *fields: ColumnRef[Any]) -> None:
        """Reload column values from the database."""
        requested = self._row_query().only(*fields)._model_fields or () if fields else ()
        self._replace_from(await self._loaded_query(*requested).get())

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
