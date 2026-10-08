"""Selected ORM statement implementation; imported only during file model setup.

It uses only the public ``orm.hooks`` and ``QuerySet`` interfaces of the ORM."""
from __future__ import annotations
from collections.abc import Generator, Mapping, Iterable
from typing import Any
from orm import QuerySet, allow_writes
from orm.hooks import prepare_insert, prepare_update
from orm_storage import Reference
from . import FileFieldError, Upload
from .model import ModelAdapter


def install_queries(model: type[Any], adapter: ModelAdapter) -> None:
    def normalized(values: Mapping[str, Any], *, allow_upload: bool = False) -> dict[str, Any]:
        out = dict(values)
        for name, value in values.items():
            if isinstance(value, Upload):
                if not allow_upload or name not in adapter.fields:
                    raise FileFieldError("Upload requires a single insert or unique-row update of a file field")
            elif name in adapter.fields:
                ref = adapter.fields[name].decode(value)
                out[name] = None if ref is None else ref.to_dict()
        return out

    def placeholders(values: Mapping[str, Any]) -> dict[str, Any]:
        return {name: Reference(adapter.fields[name].storage, "preflight").to_dict() if isinstance(value, Upload) else value
                for name, value in values.items()}

    class FileInsert:
        """``await`` uploads, then inserts; ``operation`` keeps the references for a retry."""

        def __init__(self, qs: Any, values: Mapping[str, Any]) -> None:
            values = normalized(values, allow_upload=True)
            self.qs, self.checked = qs, placeholders(values)
            # Validate every ordinary value and the statement now; protection waits for the await, like any lazy write.
            with allow_writes(qs.model):
                prepare_insert(qs, self.checked)
            self.operation = adapter.prepare_write(values, shape="insert")

        def on_conflict(self, *columns: Any) -> Any:
            raise FileFieldError("Upload is unsupported in conflict writes")

        async def run(self) -> Any:
            prepared = prepare_insert(self.qs, self.checked)
            return await self.operation.execute(prepared.execute)

        def __await__(self) -> Generator[Any, None, Any]:
            return self.run().__await__()

    class FileUpdate:
        def __init__(self, qs: Any, values: Mapping[str, Any]) -> None:
            values = normalized(values, allow_upload=True)
            self.qs, self.checked = qs, placeholders(values)
            with allow_writes(qs.model):
                if not prepare_update(qs, self.checked).unique:
                    raise FileFieldError("Upload update must target one unique row")
            self.operation = adapter.prepare_write(values, shape="unique_update")
            self.return_rows = False

        def returning(self) -> FileUpdate:
            self.return_rows = True
            return self

        async def run(self) -> Any:
            prepared = prepare_update(self.qs, self.checked)
            return await self.operation.execute(lambda data: prepared.execute(data, returning=self.return_rows))

        def __await__(self) -> Generator[Any, None, Any]:
            return self.run().__await__()

    class FileQuerySet(QuerySet[Any]):
        __slots__ = ()

        def insert(self, **values: Any) -> Any:
            if any(isinstance(value, Upload) for value in values.values()):
                return FileInsert(self, values)
            return super().insert(**normalized(values))

        def insert_many(self, rows: Iterable[Mapping[str, Any]]) -> Any:
            return super().insert_many([normalized(row) for row in rows])

        def update(self, **values: Any) -> Any:
            if any(isinstance(value, Upload) for value in values.values()):
                return FileUpdate(self, values)
            return super().update(**normalized(values))

        def update_many(self, rows: Iterable[Mapping[str, Any]], *, batch_size: int | None = None) -> Any:
            return super().update_many([normalized(row) for row in rows], batch_size=batch_size)

    model.objects = FileQuerySet(model)
