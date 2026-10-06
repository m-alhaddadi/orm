import asyncio
import io
import json
import tempfile
import tracemalloc
import unittest
from pathlib import Path
from orm_storage import Reference, LocalStorage, Registry, StorageError, CapabilityError, SizeLimitError, UploadError, UploadCancelled, CHUNK_SIZE
from orm_storage.s3 import S3Storage, PART_SIZE


class LocalTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.storage = LocalStorage("local", self.directory.name)

    async def test_bytes_stream_position_and_shared_reference(self):
        source = io.BytesIO(b"skipreport")
        source.seek(4)
        reference = await self.storage.upload(source, filename="report.xlsx")
        self.assertFalse(source.closed)
        self.assertEqual(source.tell(), 10)
        self.assertEqual(reference.size, 6)
        self.assertEqual(b"".join([c async for c in self.storage.open(reference)]), b"report")
        self.assertEqual(Reference.from_json(reference.to_json()), reference)
        self.assertIs(Registry({"local": self.storage}).resolve(reference), self.storage)
        # Merely copying/serializing/replacing a reference never deletes its object.
        self.assertTrue((Path(self.directory.name) / reference.key).exists())
        await self.storage.delete(reference)
        await self.storage.delete(reference)
        with self.assertRaises(FileNotFoundError):
            _ = [c async for c in self.storage.open(reference)]

    async def test_streaming_limit_failure_and_cancellation_cleanup(self):
        async def large():
            for _ in range(200):
                yield b"x" * CHUNK_SIZE
        ref = await self.storage.upload(large())
        self.assertEqual(ref.size, 200 * CHUNK_SIZE)
        self.assertTrue(all([len(c) <= CHUNK_SIZE async for c in self.storage.open(ref)]))
        with self.assertRaises(SizeLimitError):
            await self.storage.upload(large(), max_size=CHUNK_SIZE)
        with self.assertRaises(TypeError):
            async def invalid():
                yield "not bytes"
            await self.storage.upload(invalid())
        started = asyncio.Event()
        async def blocked():
            yield b"partial"
            started.set()
            await asyncio.Event().wait()
        task = asyncio.create_task(self.storage.upload(blocked()))
        await started.wait()
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertEqual(len(list(Path(self.directory.name).iterdir())), 1)

    async def test_bounded_memory_large_generated_stream(self):
        async def source():
            for _ in range(1024):
                yield b"x" * CHUNK_SIZE
        asyncio.get_running_loop().set_debug(False)
        tracemalloc.start()
        try:
            ref = await self.storage.upload(source())
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertEqual(ref.size, 64 * 1024 * 1024)
        self.assertLess(peak, 2 * 1024 * 1024)

    async def test_capabilities_security_and_signer(self):
        ref = await self.storage.upload(b"")
        with self.assertRaises(CapabilityError):
            await self.storage.signed_url(ref)
        signed = LocalStorage("local", self.directory.name, lambda r, expiry: f"https://files/{r.key}?ttl={expiry}")
        self.assertIn("ttl=300", await signed.signed_url(ref))
        for invalid in [Reference("other", "x"), Reference("local", "../escape")]:
            with self.assertRaises(StorageError):
                await self.storage.delete(invalid)
        target = Path(self.directory.name) / "outside"
        target.write_bytes(b"secret")
        (Path(self.directory.name) / "symlink").symlink_to(target)
        with self.assertRaises(OSError):
            _ = [c async for c in self.storage.open(Reference("local", "symlink"))]
        with self.assertRaises(ValueError):
            await signed.signed_url(ref, expires_in=0)


class FakeS3:
    def __init__(self):
        self.calls = []
        self.fail = None
    async def call(self, name, params):
        self.calls.append((name, params))
        if self.fail == name:
            raise RuntimeError(name)
        return {"UploadId": "upload", "ETag": "tag", "VersionId": "version"}
    async def create_multipart_upload(self, **kw): return await self.call("create", kw)
    async def upload_part(self, **kw): return await self.call("part", kw)
    async def complete_multipart_upload(self, **kw): return await self.call("complete", kw)
    async def abort_multipart_upload(self, **kw): return await self.call("abort", kw)
    async def delete_object(self, **kw): return await self.call("delete", kw)
    async def generate_presigned_url(self, name, **kw):
        self.calls.append((name, kw)); return "https://signed"


class S3Tests(unittest.IsolatedAsyncioTestCase):
    async def test_multipart_version_and_delegation(self):
        client = FakeS3()
        storage = S3Storage("s3", "bucket", client)
        async def source():
            for _ in range(100): yield b"x" * CHUNK_SIZE
        reference = await storage.upload(source(), filename="report")
        self.assertEqual(reference.size, 100 * CHUNK_SIZE)
        self.assertEqual(reference.version, "version")
        parts = [params for name, params in client.calls if name == "part"]
        self.assertGreaterEqual(len(parts[0]["Body"]), PART_SIZE)
        self.assertLess(len(parts[-1]["Body"]), PART_SIZE)
        self.assertEqual(await storage.signed_url(reference), "https://signed")
        await storage.delete(reference)
        self.assertEqual(client.calls[-1][1]["VersionId"], "version")

    async def test_failure_aborts_without_deletion(self):
        for failed in ["part", "complete"]:
            client = FakeS3(); client.fail = failed
            with self.assertRaises(UploadError) as failure:
                await S3Storage("s3", "bucket", client).upload(b"data")
            self.assertEqual(failure.exception.recovery.completion_unknown, failed == "complete")
            self.assertEqual(failure.exception.recovery.upload_id, "upload")
            self.assertEqual(client.calls[-1][0], "abort")
            self.assertNotIn("delete", [name for name, _ in client.calls])
        client = FakeS3()
        with self.assertRaises(UploadError) as failure:
            await S3Storage("s3", "bucket", client).upload(b"large", max_size=1)
        self.assertIsInstance(failure.exception.__cause__, SizeLimitError)
        self.assertEqual(client.calls[-1][0], "abort")

    async def test_cancellation_preserves_recovery(self):
        client = FakeS3()
        entered = asyncio.Event()
        async def pending(**kwargs):
            entered.set()
            await asyncio.Event().wait()
        client.complete_multipart_upload = pending
        task = asyncio.create_task(S3Storage("s3", "bucket", client).upload(b"data"))
        await entered.wait(); task.cancel()
        with self.assertRaises(UploadCancelled) as cancelled:
            await task
        self.assertEqual(cancelled.exception.recovery.upload_id, "upload")
        self.assertTrue(cancelled.exception.recovery.completion_unknown)
        self.assertEqual(cancelled.exception.recovery.reference.storage, "s3")
        self.assertEqual(client.calls[-1][0], "abort")

    async def test_bounded_memory_large_s3_stream(self):
        class DiscardS3(FakeS3):
            async def call(self, name, params):
                return {"UploadId": "upload", "ETag": "tag"}
        async def source():
            for _ in range(1024): yield b"x" * CHUNK_SIZE
        asyncio.get_running_loop().set_debug(False)
        tracemalloc.start()
        try:
            ref = await S3Storage("s3", "bucket", DiscardS3()).upload(source())
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertEqual(ref.size, 64 * 1024 * 1024)
        self.assertLess(peak, 16 * 1024 * 1024)


class ReferenceTests(unittest.TestCase):
    def test_shared_vectors(self):
        for data in json.loads((Path(__file__).parents[2] / "fixtures/references.json").read_text()):
            self.assertEqual(Reference.from_dict(data).to_dict(), data)
    def test_invalid(self):
        for extra in [{"v": 2}, {"size": -1}, {"size": True}, {"size": 2**53}, {"version": None}, {"storage": ""}, {"secret": "no"}, {"key": "\ud800"}]:
            with self.assertRaises(ValueError):
                Reference.from_dict({"v": 1, "storage": "local", "key": "key", **extra})

if __name__ == "__main__": unittest.main()
