"""Selected model setup shared by generated and runtime-defined bindings."""
from collections.abc import Mapping, Sequence
from typing import Any
from orm_storage import Registry
from . import FileField, FileFieldError, PreparedFileWrite
from .decoder import prepare_decoder


class ModelAdapter:
    def __init__(self, fields: Mapping[str, FileField], registry: Registry | None = None):
        self.fields = dict(fields)
        self.registry = registry

    def configure(self, registry: Registry) -> None:
        self.registry = registry

    def prepare_write(self, values: Mapping[str, Any], *, shape: str) -> PreparedFileWrite:
        if self.registry is None:
            raise FileFieldError("configure file storage before preparing uploads")
        return PreparedFileWrite(values, self.fields, self.registry, shape=shape)

    def decoder(self, public: Sequence[tuple[str, int]]) -> Any:
        return prepare_decoder(self.fields, public)


class _FileAttribute:
    """Reads a loaded file field as a Reference, without I/O; the column stays the class-side value."""
    __slots__ = ("column", "field")

    def __init__(self, column: Any, field: FileField) -> None:
        self.column = column
        self.field = field

    def __get__(self, obj: Any, owner: type[Any]) -> Any:
        if obj is None:
            return self.column.__get__(None, owner)
        try:
            value = obj.__dict__[self.field.name]
        except KeyError:
            return self.column.__get__(obj, owner)
        return self.field.decode(value)

    # A data descriptor, so that it reads before the native row values in the instance __dict__.
    def __set__(self, obj: Any, value: Any) -> None:
        raise AttributeError(f"{type(obj).__name__}.{self.field.name} is read-only")


def install_model(model: type[Any], fields: Mapping[str, FileField], registry: Registry | None = None) -> ModelAdapter:
    """Called once at definition after selected artifact validation, before publish.

    The host installs adapter.decoder(public slots) in its prepared materializer
    and adapter.prepare_write in the selected statement implementation.
    """
    adapter = ModelAdapter(fields, registry)
    methods: dict[str, Any] = {}
    for name, field in fields.items():
        def signed_url(self: Any, *, expires_in: int = 300, _field: FileField = field) -> Any:
            reference = _field.reference(vars(self))
            if adapter.registry is None:
                raise FileFieldError("configure file storage before file operations")
            return adapter.registry.resolve(reference).signed_url(reference, expires_in=expires_in)

        def open_file(self: Any, _field: FileField = field) -> Any:
            reference = _field.reference(vars(self))
            if adapter.registry is None:
                raise FileFieldError("configure file storage before file operations")
            return adapter.registry.resolve(reference).open(reference)

        methods[name + "_signed_url"] = signed_url
        methods[name + "_open"] = open_file
    # Validate the whole installation before attaching any method.
    for name in methods:
        if any(name in vars(base) for base in model.__mro__):
            raise FileFieldError(f"{model.__name__}.{name}: file method collision")
    for name, method in methods.items():
        setattr(model, name, method)
    if hasattr(model, "_meta"):
        for name, field in fields.items():
            setattr(model, name, _FileAttribute(vars(model)[name], field))
        from .orm import install_queries
        install_queries(model, adapter)
    return adapter
