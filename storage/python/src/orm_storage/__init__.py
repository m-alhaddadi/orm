"""Independent object storage. No ORM imports, global clients, or implicit I/O."""
from __future__ import annotations

import asyncio
import inspect
import json
import os
import re
import uuid
from collections.abc import AsyncIterable, AsyncIterator, Callable, Mapping
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, BinaryIO, Protocol, TypeVar

CHUNK_SIZE = 64 * 1024
MAX_SIZE = 2**53 - 1  # exact in both Python and JavaScript


class StorageError(Exception):
    """Storage configuration or operation failure."""


class CapabilityError(StorageError):
    pass


class SizeLimitError(StorageError):
    pass


@dataclass(frozen=True)
class Reference:
    storage: str
    key: str
    version: str | None = None
    filename: str | None = None
    content_type: str | None = None
    size: int | None = None

    def __post_init__(self) -> None:
        for name in ("storage", "key"):
            value = getattr(self, name)
            if not isinstance(value, str) or not value or "\x00" in value:
                raise ValueError(f"{name} must be a nonempty string without NUL")
        for name in ("version", "filename", "content_type"):
            value = getattr(self, name)
            if value is not None and (not isinstance(value, str) or "\x00" in value):
                raise ValueError(f"{name} must be a string without NUL")
        for value in (self.storage, self.key, self.version, self.filename, self.content_type):
            if value is not None:
                value.encode("utf-8")
        if self.size is not None and (type(self.size) is not int or not 0 <= self.size <= MAX_SIZE):
            raise ValueError("size must be a nonnegative safe integer")

    def to_dict(self) -> dict[str, Any]:
        return {"v": 1, **{k: v for k, v in asdict(self).items() if v is not None}}

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), ensure_ascii=False, separators=(",", ":"))

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> Reference:
        allowed = {"v", "storage", "key", "version", "filename", "content_type", "size"}
        if type(value.get("v")) is not int or value["v"] != 1 or set(value) - allowed:
            raise ValueError("invalid storage reference version or properties")
        if any(value.get(k) is None for k in value if k != "v"):
            raise ValueError("optional reference properties must be omitted, not null")
        try:
            return cls(**{k: v for k, v in value.items() if k != "v"})
        except TypeError as error:
            raise ValueError("invalid storage reference") from error

    @classmethod
    def from_json(cls, value: str) -> Reference:
        decoded = json.loads(value)
        if not isinstance(decoded, dict):
            raise ValueError("reference must be an object")
        return cls.from_dict(decoded)


@dataclass(frozen=True)
class UploadRecovery:
    reference: Reference
    upload_id: str | None = None
    completion_unknown: bool = False
    cleanup_error: BaseException | None = None


class UploadError(StorageError):
    def __init__(self, recovery: UploadRecovery):
        super().__init__(f"upload failed; recovery key={recovery.reference.key}")
        self.recovery = recovery


class UploadCancelled(asyncio.CancelledError):
    def __init__(self, recovery: UploadRecovery):
        super().__init__(f"upload cancelled; recovery key={recovery.reference.key}")
        self.recovery = recovery


Source = bytes | bytearray | memoryview | BinaryIO | AsyncIterable[bytes]


_T = TypeVar("_T")


async def _io(function: Callable[..., _T], *args: Any) -> _T:
    # Repeated cancellation must not detach a thread from its owned file handle.
    task = asyncio.create_task(asyncio.to_thread(function, *args))
    cancelled = False
    while True:
        try:
            result = await asyncio.shield(task)
            break
        except asyncio.CancelledError:
            cancelled = True
            if task.done():
                raise
    if cancelled:
        raise asyncio.CancelledError
    return result


async def chunks(source: Source, max_size: int = MAX_SIZE) -> AsyncIterator[bytes]:
    """Read current stream position; never close or rewind the caller's stream."""
    if type(max_size) is not int or not 0 <= max_size <= MAX_SIZE:
        raise ValueError("max_size must be a nonnegative safe integer")
    size = 0

    async def incoming() -> AsyncIterator[bytes]:
        if isinstance(source, (bytes, bytearray, memoryview)):
            for i in range(0, len(source), CHUNK_SIZE):
                yield bytes(source[i:i + CHUNK_SIZE])
        elif isinstance(source, AsyncIterable):
            async for chunk in source:
                yield chunk
        else:
            while True:
                chunk = await _io(source.read, CHUNK_SIZE)
                if not chunk:
                    break
                yield chunk

    async for chunk in incoming():
        if not isinstance(chunk, (bytes, bytearray, memoryview)):
            raise TypeError("upload streams must yield bytes")
        size += len(chunk)
        if size > max_size:
            raise SizeLimitError(f"upload exceeds {max_size} bytes")
        for i in range(0, len(chunk), CHUNK_SIZE):
            yield bytes(chunk[i:i + CHUNK_SIZE])


class Provider(Protocol):
    storage: str

    async def upload(self, source: Source, *, filename: str | None = None,
                     content_type: str | None = None, max_size: int = MAX_SIZE) -> Reference: ...
    def open(self, reference: Reference) -> AsyncIterator[bytes]: ...
    async def delete(self, reference: Reference) -> None: ...
    async def signed_url(self, reference: Reference, *, expires_in: int = 300) -> str: ...


def check_reference(storage: str, reference: Reference) -> None:
    if reference.storage != storage:
        raise StorageError(f"reference belongs to {reference.storage}, not {storage}")


def check_expiry(expires_in: int) -> None:
    if type(expires_in) is not int or not 1 <= expires_in <= 604800:
        raise ValueError("expires_in must be an integer from 1 to 604800 seconds")


class LocalStorage:
    """Flat object directory controlled by the application; filenames are metadata."""
    def __init__(self, storage: str, root: str | Path,
                 signer: Callable[[Reference, int], Any] | None = None):
        Reference(storage, "validation")
        self.storage = storage
        self.root = Path(root).resolve()
        self.root.mkdir(parents=True, exist_ok=True)
        self.signer = signer

    def _path(self, reference: Reference) -> Path:
        check_reference(self.storage, reference)
        if reference.version is not None:
            raise CapabilityError("local storage does not support object versions")
        if not re.fullmatch(r"[a-zA-Z0-9_-]+", reference.key):
            raise StorageError("local object keys must be flat safe identifiers")
        return self.root / reference.key

    async def upload(self, source: Source, *, filename: str | None = None,
                     content_type: str | None = None, max_size: int = MAX_SIZE) -> Reference:
        ref = Reference(self.storage, uuid.uuid4().hex, filename=filename,
                        content_type=content_type or "application/octet-stream")
        path = self._path(ref)
        size = 0
        # Joined worker writes avoid blocking the event loop and cannot race cleanup.
        output = path.open("xb")
        try:
            with output:
                async for chunk in chunks(source, max_size):
                    await _io(output.write, chunk)
                    size += len(chunk)
                    await asyncio.sleep(0)
            return Reference(ref.storage, ref.key, filename=ref.filename,
                             content_type=ref.content_type, size=size)
        except BaseException:
            path.unlink(missing_ok=True)
            raise

    async def open(self, reference: Reference) -> AsyncIterator[bytes]:
        fd = os.open(self._path(reference), os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as stream:
            while chunk := await _io(stream.read, CHUNK_SIZE):
                yield chunk
                await asyncio.sleep(0)

    async def delete(self, reference: Reference) -> None:
        self._path(reference).unlink(missing_ok=True)

    async def signed_url(self, reference: Reference, *, expires_in: int = 300) -> str:
        self._path(reference)
        check_expiry(expires_in)
        if self.signer is None:
            raise CapabilityError("local signed URLs require an application serving/signing mechanism")
        result = self.signer(reference, expires_in)
        if inspect.isawaitable(result):
            result = await result
        if not isinstance(result, str):
            raise StorageError("signer must return a URL string")
        return result


class Registry:
    def __init__(self, providers: Mapping[str, Provider]):
        if any(name != provider.storage for name, provider in providers.items()):
            raise StorageError("registry identity must match provider identity")
        self._providers = dict(providers)

    def resolve(self, reference: Reference) -> Provider:
        try:
            return self._providers[reference.storage]
        except KeyError as error:
            raise StorageError(f"unconfigured storage: {reference.storage}") from error
