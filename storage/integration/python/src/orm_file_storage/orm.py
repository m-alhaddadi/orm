"""Selected ORM statement implementation; imported only during file model setup."""
from __future__ import annotations
from collections.abc import Mapping, Iterable
from typing import Any
from orm import QuerySet
from orm.write import InsertOne, Update
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

    class FileInsert(InsertOne[Any]):
        __slots__ = ("operation",)

        def __init__(self, qs: Any, values: Mapping[str, Any]) -> None:
            values = normalized(values, allow_upload=True)
            ordinary = QuerySet.insert(qs, **placeholders(values))
            ordinary._used = True
            super().__init__(qs, ordinary._fields, ordinary._rows, ordinary._provided)
            self._used = True
            self.operation = adapter.prepare_write(values, shape="insert")
            # Validate every ordinary value and statement before the first upload.
            qs.model._meta.registry.native().validate_file_insert(qs.model._meta.name, self._fields, self._rows)
            self._used = False

        def on_conflict(self, *columns: Any) -> Any:
            self._used = True
            raise FileFieldError("Upload is unsupported in conflict writes")

        async def _execute(self) -> list[Any]:
            from orm.db import resolve
            db = resolve(self._qs._db)

            async def statement(data: Mapping[str, Any]) -> list[Any]:
                rows = [[data.get(name, value) for name, value in zip(self._fields, self._rows[0])]]
                result: list[Any] = await db._insert(self._qs.model._meta.name, self._fields, rows, None, None, None, self._qs._db)
                return result

            result: list[Any] = await self.operation.execute(statement)
            return result

    class FileUpdate:
        def __init__(self, qs: Any, values: Mapping[str, Any]) -> None:
            values = normalized(values, allow_upload=True)
            preliminary = Update.build(qs, placeholders(values))
            preliminary._used = True
            ir, params = preliminary._ir, preliminary._params
            assert ir is not None
            unique = {f.name for f in model._meta.fields.values() if f.primary_key or f.unique}

            def proves_unique(expr: Mapping[str, Any]) -> bool:
                if expr.get("t") == "and":
                    return any(proves_unique(item) for item in expr["items"])
                if expr.get("t") != "cmp" or expr.get("op") != "eq":
                    return False
                for column, parameter in [(expr.get("l", {}), expr.get("r", {})), (expr.get("r", {}), expr.get("l", {}))]:
                    if column.get("t") == "col" and not column.get("path") and column.get("name") in unique and parameter.get("t") == "param" and params[parameter["i"]] is not None:
                        return True
                return False

            if not any(proves_unique(expr) for expr in ir.get("filters", ())):
                raise FileFieldError("Upload update must target one unique row")
            import json
            model._meta.registry.native().sql(json.dumps(ir), params)
            self.operation = adapter.prepare_write(values, shape="unique_update")
            self.qs = qs
            self.return_rows = False

        def returning(self) -> FileUpdate:
            self.return_rows = True
            return self

        def __await__(self) -> Any:
            async def run() -> Any:
                async def statement(data: Mapping[str, Any]) -> Any:
                    ordinary = Update.build(self.qs, data)
                    return await ordinary.returning() if self.return_rows else await ordinary
                return await self.operation.execute(statement)
            return run().__await__()

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
