"""Actual optional SDK against a controlled S3-compatible HTTP fixture."""
import asyncio
import unittest
from orm_storage.s3 import S3Storage
try:
    from aiohttp import web
    from aiobotocore.session import get_session
except ImportError:
    web = None


@unittest.skipIf(web is None, "install the optional s3 dependencies")
class SDKTests(unittest.IsolatedAsyncioTestCase):
    async def test_transport_upload_download_sign_delete(self):
        parts = {}
        objects = {}
        calls = []
        async def handle(request):
            key = request.match_info["key"]
            calls.append((request.method, dict(request.query)))
            if request.method == "POST" and "uploads" in request.query:
                return web.Response(text=f"<InitiateMultipartUploadResult><Bucket>bucket</Bucket><Key>{key}</Key><UploadId>u1</UploadId></InitiateMultipartUploadResult>", content_type="application/xml")
            if request.method == "PUT":
                parts[int(request.query["partNumber"])] = await request.read()
                return web.Response(headers={"ETag": '"part"'})
            if request.method == "POST":
                await request.read()
                objects[key] = b"".join(parts[n] for n in sorted(parts))
                return web.Response(text=f"<CompleteMultipartUploadResult><Bucket>bucket</Bucket><Key>{key}</Key><ETag>complete</ETag></CompleteMultipartUploadResult>", content_type="application/xml", headers={"x-amz-version-id": "v1"})
            if request.method == "GET":
                return web.Response(body=objects[key])
            if request.method == "DELETE":
                objects.pop(key, None)
                return web.Response(status=204)
        app = web.Application(client_max_size=8 * 1024 * 1024)
        app.router.add_route("*", "/bucket/{key}", handle)
        runner = web.AppRunner(app); await runner.setup()
        site = web.TCPSite(runner, "127.0.0.1", 0); await site.start()
        port = site._server.sockets[0].getsockname()[1]
        try:
            session = get_session()
            async with session.create_client("s3", region_name="us-east-1", endpoint_url=f"http://127.0.0.1:{port}", aws_access_key_id="TEST", aws_secret_access_key="test-secret") as client:
                storage = S3Storage("s3", "bucket", client)
                async def source():
                    for _ in range(100): yield b"x" * 65536
                ref = await storage.upload(source(), filename="report.xlsx")
                self.assertEqual(ref.version, "v1")
                self.assertEqual(ref.size, 100 * 65536)
                size = 0
                async for chunk in storage.open(ref):
                    self.assertLessEqual(len(chunk), 65536)
                    size += len(chunk)
                self.assertEqual(size, ref.size)
                before = len(calls)
                url = await storage.signed_url(ref, expires_in=300)
                self.assertIn("versionId=v1", url)
                self.assertIn("Signature", url)
                self.assertEqual(len(calls), before)
                await storage.delete(ref)
                self.assertEqual(calls[-1][1]["versionId"], "v1")
                self.assertEqual(objects, {})
        finally:
            await runner.cleanup()
