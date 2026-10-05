import asyncio
import tempfile
import unittest
from pathlib import Path
from orm_storage import LocalStorage, Reference, Registry
from orm_file_storage import FileField, Upload, PreparedFileWrite, FileFieldError, FileNotLoaded, MissingFile, FileWriteError, FileWriteCancelled

class Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory(); self.addCleanup(self.directory.cleanup)
        self.provider = LocalStorage("reports", self.directory.name)
        self.registry = Registry({"reports": self.provider})
        self.fields = {"file": FileField("file", "reports", True)}
    async def test_deferred_prepare_retry_sql_and_outer_failure(self):
        upload = Upload(b"report", filename="report.xlsx")
        self.assertEqual(list(Path(self.directory.name).iterdir()), [])
        operation = PreparedFileWrite({"file": upload}, self.fields, self.registry)
        async def fail(data): raise RuntimeError("SQL failure or uncertain acknowledgement")
        with self.assertRaises(FileWriteError) as failure: await operation.execute(fail)
        self.assertIs(failure.exception.operation, operation)
        self.assertEqual(len(operation.references), 1)
        async def succeed(data): return data["file"]
        data = await operation.execute(succeed)
        self.assertEqual(Reference.from_dict(data), operation.references["file"])
        self.assertEqual(len(list(Path(self.directory.name).iterdir())), 1)
        # An outer rollback/failure must not remove the completed physical object.
        self.assertTrue((Path(self.directory.name) / operation.references["file"].key).exists())
    async def test_reference_only_and_unsupported_shapes_before_io(self):
        ref = await self.provider.upload(b"report")
        for shape in ["bulk", "multi_update", "upsert", "ignore", "expression"]:
            with self.assertRaises(FileFieldError):
                PreparedFileWrite({"file": Upload(b"new")}, self.fields, self.registry, shape=shape)
            operation = PreparedFileWrite({"file": ref}, self.fields, self.registry, shape=shape)
            self.assertEqual((await operation.prepare())["file"], ref.to_dict())
        self.assertEqual(len(list(Path(self.directory.name).iterdir())), 1)
        with self.assertRaises(FileFieldError):
            PreparedFileWrite({"file": Upload(b"valid"), "other": Upload(b"invalid")}, self.fields, self.registry)
        self.assertEqual(len(list(Path(self.directory.name).iterdir())), 1)
    async def test_sql_cancellation_preserves_operation(self):
        operation = PreparedFileWrite({"file": Upload(b"report")}, self.fields, self.registry)
        entered = asyncio.Event()
        async def blocked(data): entered.set(); await asyncio.Event().wait()
        task = asyncio.create_task(operation.execute(blocked)); await entered.wait(); task.cancel()
        with self.assertRaises(FileWriteCancelled) as cancelled: await task
        self.assertIs(cancelled.exception.operation, operation)
        self.assertEqual(len(operation.references), 1)
    async def test_null_and_unloaded_fail_before_provider_access(self):
        field = self.fields["file"]
        with self.assertRaises(FileNotLoaded): await field.signed_url({}, self.registry)
        with self.assertRaises(MissingFile): field.open({"file": None}, self.registry)
        self.assertIsNone(field.decode(None))
        self.assertEqual(list(Path(self.directory.name).iterdir()), [])

    async def test_partial_upload_failure_is_terminal_and_keeps_completed_reference(self):
        fields = {"first": FileField("first", "reports"), "second": FileField("second", "reports")}
        async def invalid():
            yield "invalid bytes"
        operation = PreparedFileWrite({"first": Upload(b"first"), "second": Upload(invalid())}, fields, self.registry)
        with self.assertRaises(FileWriteError): await operation.prepare()
        self.assertEqual(set(operation.references), {"first"})
        with self.assertRaises(FileFieldError): await operation.prepare()
        self.assertEqual(len(list(Path(self.directory.name).iterdir())), 1)
