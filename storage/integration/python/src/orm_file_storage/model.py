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


def install_model(model: type[Any], fields: Mapping[str, FileField], registry: Registry | None = None) -> ModelAdapter:
    """Called once at definition after selected artifact validation, before publish.

    It uses the public ``orm.hooks``: ``decode_field`` for loaded file fields, and
    ``prepare_insert`` and ``prepare_update`` in the file statements.
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
        from orm.hooks import decode_field
        for name, field in fields.items():
            # A loaded file field reads as a Reference, without I/O.
            decode_field(model, name, field.decode)
        from .orm import install_queries
        install_queries(model, adapter)
    return adapter
