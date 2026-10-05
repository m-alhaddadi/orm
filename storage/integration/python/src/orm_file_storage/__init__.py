"""Selected file-field binding helpers. No native/provider callback registry."""
from __future__ import annotations
import asyncio
from collections.abc import AsyncIterator, Awaitable, Callable, Mapping
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any, TypeVar
from orm_storage import Reference, Registry, Source, MAX_SIZE, UploadRecovery, UploadError, UploadCancelled


class FileFieldError(ValueError):
    pass


class FileNotLoaded(FileFieldError):
    pass


class MissingFile(FileFieldError):
    pass


@dataclass(frozen=True)
class Upload:
    """Deferred write input: construction performs no I/O."""
    source: Source
    filename: str | None = None
    content_type: str | None = None
    max_size: int = MAX_SIZE

    def __post_init__(self) -> None:
        from collections.abc import AsyncIterable
        if not isinstance(self.source, (bytes, bytearray, memoryview, AsyncIterable)) and not callable(getattr(self.source, "read", None)):
            raise TypeError("Upload requires bytes or a byte stream")
        Reference("validation", "validation", filename=self.filename, content_type=self.content_type)
        if type(self.max_size) is not int or not 0 <= self.max_size <= MAX_SIZE:
            raise FileFieldError("max_size must be a nonnegative safe integer")


@dataclass(frozen=True)
class FileField:
    name: str
    storage: str
    nullable: bool = False

    def decode(self, value: Any) -> Reference | None:
        if value is None:
            if not self.nullable:
                raise MissingFile(f"{self.name} is not nullable")
            return None
        reference = value if isinstance(value, Reference) else Reference.from_dict(value)
        if reference.storage != self.storage:
            raise FileFieldError(f"{self.name} requires storage {self.storage}")
        return reference

    def reference(self, snapshot: Mapping[str, Any]) -> Reference:
        if self.name not in snapshot:
            raise FileNotLoaded(f"{self.name} was not selected")
        reference = self.decode(snapshot[self.name])
        if reference is None:
            raise MissingFile(f"{self.name} has no file")
        return reference

    async def signed_url(self, snapshot: Mapping[str, Any], registry: Registry, *, expires_in: int = 300) -> str:
        reference = self.reference(snapshot)
        return await registry.resolve(reference).signed_url(reference, expires_in=expires_in)

    def open(self, snapshot: Mapping[str, Any], registry: Registry) -> AsyncIterator[bytes]:
        reference = self.reference(snapshot)
        return registry.resolve(reference).open(reference)


_T = TypeVar("_T")


class FileWriteError(Exception):
    def __init__(self, operation: PreparedFileWrite):
        super().__init__("file write failed; inspect operation.references and operation.recovery")
        self.operation = operation


class FileWriteCancelled(asyncio.CancelledError):
    def __init__(self, operation: PreparedFileWrite):
        super().__init__("file write cancelled; inspect operation.references and operation.recovery")
        self.operation = operation


class PreparedFileWrite:
    """Single-row binding preparation. Caller still owns the SQL/outer transaction.

    execute receives a fixed native statement runner, never a provider callback.
    Its completed JSON values can be reused for explicit SQL retries. Failed or
    cancelled source preparation is terminal to avoid replaying caller streams.
    """
    def __init__(self, values: Mapping[str, Any], fields: Mapping[str, FileField],
                 registry: Registry, *, shape: str = "insert"):
        self._values = dict(values)
        self._fields = dict(fields)
        self._registry = registry
        self._references: dict[str, Reference] = {}
        self._recovery: list[UploadRecovery] = []
        self._data: dict[str, Any] = dict(values)
        self._state = "new"
        self._executing = False
        # Validate the complete batch before any provider call.
        for name, value in self._values.items():
            field = fields.get(name)
            if field is None:
                if isinstance(value, Upload):
                    raise FileFieldError(f"{name} is not a file field")
                continue
            if isinstance(value, Upload):
                if shape not in ("insert", "unique_update"):
                    raise FileFieldError(f"Upload is unsupported for {shape}")
                Reference(field.storage, "validation", filename=value.filename,
                          content_type=value.content_type)
                if type(value.max_size) is not int or not 0 <= value.max_size <= MAX_SIZE:
                    raise FileFieldError("max_size must be a nonnegative safe integer")
                registry.resolve(Reference(field.storage, "validation"))
            else:
                reference = field.decode(value)
                self._data[name] = reference.to_dict() if reference is not None else None

    @property
    def references(self) -> Mapping[str, Reference]:
        return MappingProxyType(self._references)

    @property
    def recovery(self) -> tuple[UploadRecovery, ...]:
        return tuple(self._recovery)

    async def prepare(self) -> Mapping[str, Any]:
        if self._state == "ready":
            return MappingProxyType(self._data)
        if self._state != "new":
            raise FileFieldError("upload preparation already running or failed; sources cannot be replayed")
        self._state = "preparing"
        try:
            for name, value in self._values.items():
                if not isinstance(value, Upload):
                    continue
                field = self._fields[name]
                provider = self._registry.resolve(Reference(field.storage, "validation"))
                reference = await provider.upload(value.source, filename=value.filename,
                    content_type=value.content_type, max_size=value.max_size)
                # No await between receipt and publishing recovery data.
                self._references[name] = reference
                self._data[name] = reference.to_dict()
            self._state = "ready"
            return MappingProxyType(self._data)
        except BaseException as error:
            self._state = "failed"
            if isinstance(error, (UploadError, UploadCancelled)):
                self._recovery.append(error.recovery)
            if isinstance(error, asyncio.CancelledError):
                raise FileWriteCancelled(self) from error
            raise FileWriteError(self) from error

    async def execute(self, statement: Callable[[Mapping[str, Any]], Awaitable[_T]]) -> _T:
        if self._executing:
            raise FileFieldError("file write already executing")
        self._executing = True
        try:
            data = await self.prepare()
            return await statement(data)
        except (FileWriteError, FileWriteCancelled):
            raise
        except asyncio.CancelledError as error:
            raise FileWriteCancelled(self) from error
        except BaseException as error:
            raise FileWriteError(self) from error
        finally:
            self._executing = False
