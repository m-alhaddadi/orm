"""S3-compatible adapter for an application-owned async aiobotocore client.

Importing this module does not import or require an SDK. The client may instead
be any compatible async transport, useful for tests and additional providers.
"""
from __future__ import annotations
import asyncio
import inspect
import uuid
from collections.abc import AsyncIterator
from typing import Any
from . import MAX_SIZE, Reference, Source, StorageError, UploadRecovery, UploadError, UploadCancelled, check_expiry, check_reference, chunks

PART_SIZE = 5 * 1024 * 1024


class S3Storage:
    def __init__(self, storage: str, bucket: str, client: Any):
        Reference(storage, "validation")
        if not bucket:
            raise ValueError("bucket must be nonempty")
        self.storage, self.bucket, self.client = storage, bucket, client

    def _params(self, reference: Reference) -> dict[str, Any]:
        check_reference(self.storage, reference)
        params: dict[str, Any] = {"Bucket": self.bucket, "Key": reference.key}
        if reference.version is not None:
            params["VersionId"] = reference.version
        return params

    async def upload(self, source: Source, *, filename: str | None = None,
                     content_type: str | None = None, max_size: int = MAX_SIZE) -> Reference:
        ref = Reference(self.storage, uuid.uuid4().hex, filename=filename,
                        content_type=content_type or "application/octet-stream")
        params = self._params(ref)
        upload_id: str | None = None
        completing = False
        parts: list[dict[str, Any]] = []
        buffer = bytearray()
        size = 0

        async def part(data: bytes) -> None:
            number = len(parts) + 1
            if number > 10000:
                raise StorageError("S3 multipart limit exceeded")
            response = await self.client.upload_part(**params, UploadId=upload_id,
                                                     PartNumber=number, Body=data)
            parts.append({"PartNumber": number, "ETag": response["ETag"]})

        try:
            created = await self.client.create_multipart_upload(**params, ContentType=ref.content_type)
            upload_id = created["UploadId"]
            async for chunk in chunks(source, max_size):
                buffer.extend(chunk)
                size += len(chunk)
                if len(buffer) >= PART_SIZE:
                    await part(bytes(buffer))
                    buffer.clear()
            if buffer or not parts:
                await part(bytes(buffer))
            completing = True
            completed = await self.client.complete_multipart_upload(
                **params, UploadId=upload_id, MultipartUpload={"Parts": parts})
            return Reference(ref.storage, ref.key, version=completed.get("VersionId"),
                             filename=ref.filename, content_type=ref.content_type, size=size)
        except BaseException as error:
            # Abort only unfinished multipart state, never delete a possibly committed object.
            cleanup_error = None
            if upload_id is not None:
                try:
                    await asyncio.shield(self.client.abort_multipart_upload(**params, UploadId=upload_id))
                except BaseException as abort_error:
                    cleanup_error = abort_error
            recovery = UploadRecovery(ref, upload_id, completing, cleanup_error)
            if isinstance(error, asyncio.CancelledError):
                raise UploadCancelled(recovery) from error
            raise UploadError(recovery) from error

    async def open(self, reference: Reference) -> AsyncIterator[bytes]:
        response = await self.client.get_object(**self._params(reference))
        async with response["Body"] as body:
            while chunk := await body.read(64 * 1024):
                yield chunk

    async def delete(self, reference: Reference) -> None:
        await self.client.delete_object(**self._params(reference))

    async def signed_url(self, reference: Reference, *, expires_in: int = 300) -> str:
        check_expiry(expires_in)
        value = self.client.generate_presigned_url("get_object", Params=self._params(reference),
                                                   ExpiresIn=expires_in)
        if inspect.isawaitable(value):
            value = await value
        if not isinstance(value, str):
            raise StorageError("signer must return a URL string")
        return value
