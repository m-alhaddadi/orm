# Independent file storage

Three independently packaged modules live here. None imports or depends on the
ORM: `python/` installs as `orm-storage` (`orm_storage`), `typescript/` packs as
`@orm/storage`, and `reference/` is the `storage-reference` Rust wire codec.
Each directory has its own package manifest. To extract them, copy `storage/`
(including `fixtures/`) into a repository of your choice. Runtime packages need
only their own directory; fixture files are for development tests.

The ORM extension is a separate integration deliverable. The packages below are
usable now without ORM installation, model configuration, or native builds.

## Python

```python
from contextlib import aclosing
from orm_storage import LocalStorage, Reference

storage = LocalStorage("reports", "/srv/reports")
stored = await storage.upload(report_bytes, filename="report.xlsx")
# Persist stored.to_dict() in any JSON database column.
reference = Reference.from_dict(database_value)
async with aclosing(storage.open(reference)) as stream:
    async for chunk in stream:
        await response.write(chunk)
await storage.delete(reference)  # explicit application ownership decision
```

`upload` accepts bytes, bytearray, memoryview, binary file objects and async byte
iterables. Binary file objects are read from their current position, without
rewinding or closing them. Caller-owned async iterators are not explicitly closed.
Cancel an awaited operation using ordinary asyncio cancellation. Download streams
are provider owned; close them when breaking early (`aclosing` above).

For S3, install the package's `s3` extra. Own the configured SDK client's lifetime:

```python
from aiobotocore.session import get_session
from orm_storage.s3 import S3Storage

async with get_session().create_client(
    "s3", region_name="us-east-1", endpoint_url="https://objects.example.com",
) as client:
    storage = S3Storage("reports", "report-bucket", client)
    reference = await storage.upload(report_stream, filename="report.pdf")
    url = await storage.signed_url(reference, expires_in=300)
```

SDK credentials come from application configuration or the SDK credential chain.
The SDK is not imported by the base package or even the Python S3 adapter: it
accepts a compatible application-owned async client.

## TypeScript (Node 20+)

```typescript
import { LocalStorage, Reference } from "@orm/storage";

const storage = new LocalStorage("reports", "/srv/reports");
const reference = await storage.upload(reportBytes, { filename: "report.xlsx" });
const storedJSON = JSON.stringify(reference);
const restored = Reference.fromJSON(storedJSON);
for await (const chunk of storage.open(restored)) {
  await response.write(chunk);
}
```

Upload sources are Uint8Array (including Buffer) or async Uint8Array iterables.
Pass `{ signal, maxSize }` to cancel or limit uploads. The adapter does not invoke
`return()` on caller upload iterators. With a Node Readable, use its iterator with
`destroyOnReturn: false` if the application also plans to consume it independently.

Import `S3Storage` from `@orm/storage/s3` and supply an application-owned `S3Client`.
Install the optional peers `@aws-sdk/client-s3` and
`@aws-sdk/s3-request-presigner`. The base entrypoint has no SDK imports.
Configure endpoint, region, forcePathStyle and credentials on that SDK client.

## Provider contract

All providers implement upload/open/delete/signed_url (TypeScript `signedUrl`).
`Registry` resolves a Reference's logical storage identity. Additional providers
implement this interface without ORM provider-specific changes. Each provider
rejects references belonging to another storage identity.

Local storage uses a canonical application-controlled directory and unique flat
keys. Caller filenames are metadata, never paths. Reads reject symlinks; deletion
unlinks the object entry and never follows a symlink. The configured root and its
ancestors must be controlled by the application. Local signed URLs require an
application serving/signing callback; without one, signing raises CapabilityError.
Local object versions are unsupported. The adapter performs no existence check
when signing. Application signers own URL routing, signature validation and expiry.

Content type defaults to `application/octet-stream`; it is not inferred from an
untrusted filename. `max_size`/`maxSize` applies while consuming bytes; over-limit
uploads remove partial local files or abort unfinished multipart uploads. Local
chunks are at most 64 KiB. S3 uploads maintain one 5 MiB multipart buffer and upload
one part at a time, with a 10,000-part limit. Memory does not scale with the total
file size; memory already held by a caller source and SDK buffers is additional.
No automatic source replay or upload retry is promised. SDK retries of a buffered
part are SDK policy; the SDK never receives the caller's entire source stream.
Empty objects are supported. Downloads stream and release provider-owned bodies.

The multipart limits follow the [S3 multipart contract](https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html).
The optional JavaScript signing adapter uses the [SDK's presigner](https://docs.aws.amazon.com/sdk-for-javascript/v3/developer-guide/migrate-s3.html).

## Durable wire contract

```json
{"v":1,"storage":"reports","key":"unique-object-key","version":"optional-provider-version","filename":"report.xlsx","content_type":"application/octet-stream","size":42}
```

Optional properties are omitted, never null. Unknown properties/versions fail
validation. Size is an integer between 0 and 2^53-1 for exact cross-language
serialization. References contain no credentials, client instances or expiring
URLs. A storage identity must keep pointing at the same historical object namespace;
changing a configured bucket does not migrate previously uploaded objects.

References convey location, not authorization or exclusive ownership. Applications
must authorize externally supplied references before attaching/reading/deleting
them. Sharing a reference is allowed. Serialization, reference reads/writes,
replacement and clearing never delete or upload physical objects. Explicit deletion
is an application decision. Version-aware S3 downloads, deletion and signing pass
VersionId through to the SDK; deleting an unversioned reference follows provider
bucket/versioning semantics.

## Failure and recovery

Local upload failure/cancellation removes its partial file. S3 failures expose
`UploadError.recovery`, including the candidate Reference, known upload ID,
`completion_unknown` (`completionUnknown`) and any abort error. Python cancelled
uploads raise `UploadCancelled`, an asyncio.CancelledError subclass with the same
recovery details. TypeScript UploadError preserves the original SDK/AbortError in
`cause`. If completion acknowledgement fails or is cancelled, the object may have
committed: the adapter aborts only multipart state and **never deletes the object**.
The candidate reference may lack version/size metadata when acknowledgement was
lost. Applications may reconcile by key or apply their retention policy.

A failed multipart initialization may leave provider multipart state without a
known upload ID; the candidate key is still retained. Configure a provider lifecycle
rule to expire incomplete multipart state if appropriate. SDK clients may make
retries, but these packages do not claim exactly-once physical creation.

There is no SQL/storage atomic transaction. Independently upload, retain the returned
reference, then persist its dictionary/JSON. SQL failure, cancellation, rollback,
savepoint rollback, uncertain commit acknowledgement or outer transaction failure
can leave an unattached object. Never infer that destructive cleanup is safe from
an uncertain SQL outcome. Reuse completed references for SQL retries.

## Verification

Tests use one shared cross-language fixture set and controllable provider failures.
The Python optional-SDK test uses aiobotocore against a temporary localhost S3 HTTP
fixture; it verifies actual multipart serialization, streaming download, version
forwarding, signing and deletion. This is not a production S3 service acceptance
claim. The TypeScript suite verifies real SDK signing and controlled multipart
commands. Python memory tests stream 64 MiB and cap traced peak allocations at
2 MiB local / 16 MiB multipart (caller-generated chunks; transport discards parts).

The root `CLAUDE.md` checks build without `file-storage` and do not run these packages.
Run them too when you change `storage/` or the file-storage host code:

```bash
cargo test -q -p orm-core -p orm-engine --features file-storage
cargo test -q --manifest-path storage/reference/Cargo.toml
cargo test -q --manifest-path storage/orm-extension/Cargo.toml
uv pip install -e storage/python -e storage/integration/python
python -m pytest -q storage/python/tests storage/integration/python/tests
(cd storage/typescript && npm install && npm test)
(cd storage/integration/typescript && npm install && npm test)
# End-to-end, against a file-storage build; set FILE_STORAGE_DATABASE_URL for Postgres.
maturin develop --features file-storage && python storage/integration/tests/native_python.py
cargo build -p orm-node --features file-storage && cp target/debug/liborm_node.dylib js/orm.node
(cd js && npm run build) && node --test storage/integration/tests/native_node.mjs
```

Rebuild the default bindings (`maturin develop`, `npm run build:native`) before the root checks.
