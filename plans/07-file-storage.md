# Independent storage library and ORM file-field integration

Status: implementation proposal, not implemented behavior. Names, package layout,
and schema declarations are proposals.

## Outcome

Build storage as a separate project usable without this ORM. An optional ORM
extension composes its interface with model fields to upload files during explicit
writes and generate signed URLs or download streams from stored references.

Generated images, Excel reports, PDFs, and application uploads are initial use
cases. Python and TypeScript expose equivalent behavior. The storage project can
proceed independently; the ORM extension depends on plans 1 and 2.

## Modules and dependency direction

| Module | Owns |
| --- | --- |
| Independent storage library | Upload/download streams, deletion, signing, references, provider capabilities and errors |
| Storage provider adapters | Local/S3-compatible implementations, credentials, endpoints, optional SDK dependencies |
| Optional ORM extension | Field codecs, generated field operations, upload-on-write and SQL transaction coordination |
| Application | Configuration, authorization, HTTP handling, retention and cleanup policy |

The extension depends on the storage library's public interface. The storage
library has no dependency on ORM models, schemas, migrations, or transactions.
Providers can ship as optional packages in the storage project. Adding a provider
requires no ORM-specific adapter when it satisfies the supported interface.

Inject configured storage clients at initialization. Keep SDK calls and provider
branches out of the ORM planner and codecs. Disabled builds exclude integration
and storage dependencies without per-query feature checks.

## Independent composition

```python
stored = await storage.upload(file_binary, filename="report.xlsx")
report = await Report.objects.insert(file=stored)
url = await storage.signed_url(report.file, expires_in=300)
```

A reference contains a logical storage identity and object key, with optional
provider version identity and filename/content-type/size metadata. Define a
versioned serialization contract without credentials or clients. Persist durable
references; generate expiring URLs on request.

Runtime configuration resolves storage identities. Changing a bucket does not
move old objects. Application code must authorize externally supplied references;
knowing a key does not establish permission to attach, read, or delete it.

## ORM extension field operations

```python
report = await Report.objects.insert(
    file=Upload(file_binary, filename="report.xlsx")
)
await Report.objects.where(Report.id == report.id).update(
    file=Upload(new_binary, filename="report.xlsx")
)

url = await report.file_signed_url(expires_in=300)
stream = await report.file_open()
```

```typescript
const report = await Report.objects.insert({
  file: new Upload(fileBinary, { filename: "report.xlsx" }),
});
const url = await report.fileSignedUrl({ expiresIn: 300 });
const stream = await report.fileOpen();
```

Generated method names are illustrative. Validate collisions with model members
and specify missing/unloaded-field errors before implementation. Signing may be
local computation or provider I/O; an explicit async interface supports either.
Unsupported operations raise a capability error, such as private signed URLs on
local storage without a configured serving mechanism.

Synchronous field access returns a typed storage reference without I/O. Setting a
field means supplying its value to an awaited insert/update statement. Preserve
read-only snapshots; do not add assignment followed by `save()`. Updates leave
existing snapshots unchanged until queried again.

`Upload` is a deferred integration input; construction performs no I/O. Raw bytes
remain distinct from database binary values. Existing storage references are valid
write inputs and cause no upload. Nullable fields accept `None`/`null`.

The storage library owns bytes/stream handling, content-type fallback, size limits,
stream position/ownership, cancellation, and replayability. The extension reuses
those contracts. Avoid whole-file buffering and implicitly closing caller streams.

## Schema and persistence

Use namespaced extension declarations to identify file fields and select logical
storage configuration. Credentials, endpoints, buckets, and clients belong in
runtime configuration. Exact declaration syntax remains open.

Encode references in ordinary SQL columns. Choose a JSON representation or an
explicit column group during schema design. Basic reference persistence requires
no attachment table, ownership registry, or cleanup worker. Optional metadata
models remain application models or belong to a later managed lifecycle feature.

Return the storage library's reference type. Decode selected SQL values without
storage requests or extra queries per row. Preserve partial selection. Define
queryable properties and cross-language/database serialization, including provider
version identity, before release.

## Lifecycle and failure contract

Reference persistence and physical ownership are separate concerns. Basic
integration permits shared references and never automatically deletes objects
when rows are deleted, replaced, or cleared. Applications choose retention and
cleanup policy. Signing does not prove an object exists; existence checks require
an explicit storage operation.

For upload-on-write, upload under a unique key first, then persist its reference.
SQL and storage do not share an atomic transaction:

- Upload failure prevents writing the new reference.
- SQL failure, cancellation, or rollback can leave an unattached uploaded object.
- Statement success inside a transaction does not imply the outer commit succeeded.
- An uncertain commit acknowledgement must not trigger destructive cleanup.

Document these outcomes and retain the uploaded reference in a defined operation
result/error or recovery mechanism when available. Finalize this contract before
shipping convenience writes. Independent upload plus reference persistence remains
available when applications need direct control.

SQL retries reuse completed references rather than replaying uploads. Do not
promise automatic upload retries for non-replayable streams. Initially support
new uploads in single-row inserts and updates targeting one unique row. Reject
uploads in bulk inserts, multi-row updates, upserts, and conflict-ignore writes
before I/O until their semantics are defined. Reference-only writes retain normal
SQL semantics, including sharing a reference across rows.

A later opt-in lifecycle integration can add upload tracking, ownership rules,
cleanup outbox, and workers. It must handle outer transactions, savepoints,
concurrent replacement, cascades, crashes, and uncertain commits. Recovery records
must survive rollback, including SQLite writer constraints. Deletion requires an
explicit ownership contract and proof that no live owner/upload remains; a timeout
alone is insufficient. Managed lifecycle is not required for basic field access.

## Delivery

1. Define the independent storage interface, reference serialization, provider
   capabilities/errors, and streaming contracts.
2. Implement local and S3-compatible adapters in that project, with optional
   dependencies and standalone examples/tests.
3. Add ORM schema/code generation and reference codecs. Deliver standalone upload
   plus ordinary reference persistence first.
4. Add generated signing/opening operations using injected storage clients.
5. Finalize upload-on-write failure/recovery semantics and add `Upload` for supported
   write shapes.
6. Document Python/TypeScript report examples, configuration, authorization, and
   lifecycle limitations.

Later work includes managed cleanup, direct browser uploads, resumable transfers,
image variants, previews, and attachment collections. Place each feature in the
module that owns its behavior.

## Acceptance

- Storage works without installing/importing the ORM. References round-trip in
  both bindings and databases.
- Local and S3-compatible adapters satisfy one interface. A compatible additional
  provider needs no ORM provider-code change.
- Reads cause no storage calls; reference writes cause no uploads. Explicit field
  operations delegate to the configured client.
- Upload generated bytes and large streams with bounded memory; reject unsupported
  write shapes before I/O.
- Verify upload/SQL failure, cancellation, rollback, savepoints, uncertain commit,
  and SQL retries against documented convenience-write semantics.
- Basic row writes/deletes cause no automatic physical deletion. Check shared
  references, missing/unloaded fields, name collisions, and capability errors.
- Disabled builds exclude dependencies and add no query/write/decoding dispatch.

Test provider behavior in the storage project. ORM integration tests exercise
reference mapping, delegation, and transaction coordination through public
interfaces with controllable failures.

## References

- [Django FileField](https://docs.djangoproject.com/en/5.2/ref/models/fields/#filefield)
  combines a model field with a configurable storage interface.
- [Rails Active Storage](https://guides.rubyonrails.org/active_storage_overview.html)
  demonstrates a richer managed attachment lifecycle.
- [Laravel storage](https://laravel.com/docs/12.x/filesystem#file-uploads)
  returns a path for separate database persistence.
- [Prisma upload example](https://www.prisma.io/blog/fullstack-nextjs-graphql-prisma-4-1k1kc83x3v)
  separates upload from reference persistence.
