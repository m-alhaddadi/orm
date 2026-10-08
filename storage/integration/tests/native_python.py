"""Public selected-native tests; run with the worktree's own installed wheel."""
import json
import asyncio
import os
from tempfile import TemporaryDirectory
import unittest
import orm
from orm_file_storage import FileField, FileFieldError, FileWriteError, FileWriteCancelled, Upload
from orm_file_storage.model import install_model
from orm_storage import LocalStorage, Reference, Registry


class CountingStorage(LocalStorage):
    uploads = 0
    deletes = 0
    async def upload(self, *args, **kwargs):
        self.uploads += 1
        return await super().upload(*args, **kwargs)
    async def delete(self, reference):
        self.deletes += 1
        return await super().delete(reference)


class NativeFileTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = TemporaryDirectory()
        self.storage = CountingStorage('reports', self.directory.name)
        self.registry = Registry({'reports': self.storage})
        self.url = os.environ.get('FILE_STORAGE_DATABASE_URL', 'sqlite://:memory:')
        ir = json.loads(orm._native.compile_schema('model FileReport07 {\n id Int @id\n file Json?\n}', None))
        ir['dialect'] = 'sqlite' if self.url.startswith('sqlite:') else 'postgres'
        ir['behavior'] = {'schema_contract': 1, 'field_adapters': [{'model': 'FileReport07', 'field': 'file', 'adapter': 'file-storage.reference.v1'}], 'file_fields': [{'model': 'FileReport07', 'field': 'file', 'storage': 'reports', 'reference_contract': 1}]}
        reg = orm.Registry()
        self.Report = orm.define(ir, registry=reg)['FileReport07']
        self.adapter = install_model(self.Report, {'file': FileField('file', 'reports', True)}, self.registry)
        self.db = await orm.connect(self.url, registry=reg)
        await self.db.drop_tables()
        await self.db.create_tables()

    async def asyncTearDown(self):
        await self.db.drop_tables()
        await self.db.close()
        self.directory.cleanup()

    async def test_reference_shared_null_and_explicit_operations(self):
        reference = await self.storage.upload(b'report')
        inserted = await self.Report.objects.insert(id=1, file=reference)
        self.assertEqual(inserted.file, reference)
        await self.Report.objects.insert_many([{'id': 2, 'file': reference}, {'id': 3, 'file': None}])
        rows = await self.Report.objects.order_by(self.Report.id)
        self.assertEqual(self.storage.uploads, 1)
        self.assertEqual(b''.join([chunk async for chunk in rows[0].file_open()]), b'report')
        await self.Report.objects.filter(self.Report.id == 1).update(file=None)
        await self.Report.objects.filter(self.Report.id == 2).delete()
        self.assertEqual(self.storage.deletes, 0)
        self.assertEqual(b''.join([chunk async for chunk in self.storage.open(reference)]), b'report')
        with self.assertRaises(FileFieldError):
            rows[2].file_open()
        self.assertIsInstance(rows[0].file, Reference)
        self.assertEqual(rows[0].file, reference)
        self.assertIsNone(rows[2].file)

    async def test_unsupported_and_invalid_values_before_upload(self):
        for call in [lambda: self.Report.objects.insert_many([{'id': 1, 'file': Upload(b'x')}]),
                     lambda: self.Report.objects.update(file=Upload(b'x')),
                     lambda: self.Report.objects.insert(id=1, file=Upload(b'x')).on_conflict(self.Report.id, update=False),
                     lambda: self.Report.objects.insert(id='invalid integer', file=Upload(b'x'))]:
            with self.assertRaises((FileFieldError, ValueError, TypeError)):
                call()
        self.assertEqual(self.storage.uploads, 0)

    async def test_sql_failure_retry_keeps_reference_and_unique_update(self):
        await self.Report.objects.insert(id=1, file=None)
        statement = self.Report.objects.insert(id=1, file=Upload(b'generated'))
        with self.assertRaises(FileWriteError) as caught:
            await statement
        self.assertIs(caught.exception.operation, statement.operation)
        reference = statement.operation.references['file']
        await statement.operation.execute(lambda data: self.Report.objects.insert(**{**data, 'id': 2}))
        self.assertEqual(self.storage.uploads, 1)
        self.assertEqual(self.storage.deletes, 0)
        await self.Report.objects.filter(self.Report.id == 2).update(file=Upload(b'replacement'))
        self.assertEqual(self.storage.uploads, 2)
        self.assertEqual(b''.join([chunk async for chunk in self.storage.open(reference)]), b'generated')

    async def test_uncertain_statement_ack_and_cancel_keep_references(self):
        native_insert = self.db._insert
        async def uncertain(*args, **kwargs):
            await native_insert(*args, **kwargs)
            raise RuntimeError('lost SQL acknowledgement')
        self.db._insert = uncertain
        operation = self.Report.objects.insert(id=1, file=Upload(b'uncertain'))
        with self.assertRaises(FileWriteError):
            await operation
        self.db._insert = native_insert
        self.assertEqual(await self.Report.objects.count(), 1)
        self.assertEqual(self.storage.deletes, 0)
        self.assertEqual(b''.join([chunk async for chunk in self.storage.open(operation.operation.references['file'])]), b'uncertain')
        reached = asyncio.Event()
        async def pending(*args, **kwargs):
            await native_insert(*args, **kwargs)
            reached.set()
            await asyncio.Event().wait()
        self.db._insert = pending
        cancelled = self.Report.objects.insert(id=2, file=Upload(b'cancelled'))
        async def execute():
            return await cancelled
        task = asyncio.create_task(execute())
        await reached.wait()
        task.cancel()
        with self.assertRaises(FileWriteCancelled) as error:
            await task
        self.db._insert = native_insert
        self.assertIs(error.exception.operation, cancelled.operation)
        self.assertEqual(await self.Report.objects.count(), 2)
        self.assertEqual(self.storage.deletes, 0)
        self.assertEqual(b''.join([chunk async for chunk in self.storage.open(cancelled.operation.references['file'])]), b'cancelled')

    async def test_outer_rollback_and_savepoint_retain_objects(self):
        operation = self.Report.objects.insert(id=1, file=Upload(b'rollback'))
        with self.assertRaises(RuntimeError):
            async with self.db.transaction():
                await operation
                raise RuntimeError('outer rollback')
        self.assertEqual(await self.Report.objects.count(), 0)
        reference = operation.operation.references['file']
        self.assertEqual(b''.join([chunk async for chunk in self.storage.open(reference)]), b'rollback')
        async with self.db.transaction():
            await self.Report.objects.insert(id=2, file=reference)
            nested = self.Report.objects.insert(id=3, file=Upload(b'savepoint'))
            with self.assertRaises(RuntimeError):
                async with self.db.transaction():
                    await nested
                    raise RuntimeError('savepoint rollback')
        self.assertEqual(await self.Report.objects.count(), 1)
        self.assertEqual(self.storage.deletes, 0)
        self.assertEqual(b''.join([chunk async for chunk in self.storage.open(nested.operation.references['file'])]), b'savepoint')

if __name__ == '__main__':
    unittest.main()
