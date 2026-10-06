# File-field binding preparation

`python/` and `typescript/` contain separately packaged selected binding helpers.
They depend on the independent storage package. They do not import ORM internals,
provider SDKs, or install a runtime native callback registry.

`install_model` / `installModel` decode loaded file fields to `Reference` and route
`Upload` writes through `PreparedFileWrite`. Annotated file fields need a native
artifact built with the `file-storage` feature; selecting `../orm-extension/` in
`orm-extension-build` turns it on. Installing these helpers alone does not.

The selected declaration is proposed as:

```prisma
model Report {
  id Int @id @default(autoincrement())
  file Json? @storage.file(storage: "reports")
}
```

The compiler contributes a fixed `file-storage.reference.v1` field adapter plus
logical storage identity/reference contract metadata. It rejects non-scalar JSON,
database-defaulted/key fields and generated member collisions. Credentials and
configured providers belong to the application's Registry.

## Prepared operation

```python
from orm_file_storage import FileField, PreparedFileWrite, Upload

operation = PreparedFileWrite(
    {"file": Upload(report_bytes, filename="report.xlsx")},
    {"file": FileField("file", "reports")},
    storage_registry,
)
report = await operation.execute(lambda values: Report.objects.insert(**values))
# operation.references remains inspectable after the enclosing transaction fails.
```

```typescript
import { FileField, PreparedFileWrite, Upload } from "@orm/file-storage";

const operation = new PreparedFileWrite(
  { file: new Upload(reportBytes, { filename: "report.xlsx" }) },
  new Map([["file", new FileField("file", "reports")]]),
  storageRegistry,
);
const report = await operation.execute(values => Report.objects.insert(values));
```

These examples explicitly persist ordinary JSON; field-specific generated write
types must be supplied by the selected ORM adapter before automatic Upload writes
are available. Deferred Upload construction makes no I/O calls. All input and
write-shape checks precede uploads. The trusted adapter must prove a unique-row
update before passing `unique_update`; an arbitrary filter is insufficient.
Bulk, multi-row update, upsert, conflict-ignore and expression shapes reject Upload
before provider I/O. Existing Reference/null values retain normal SQL semantics.

`.prepare()` uploads once and returns JSON inputs; `.execute(statement)` passes
those inputs to an awaited statement. A failed/cancelled preparation is terminal,
so consumed sources are never automatically replayed. Completed preparations can
be reused for explicit SQL retries without another upload. Concurrent execution
of the same operation is rejected. There is no automatic SQL retry loop.

`references` retains completed uploaded references, including when a later upload
fails. `recovery` retains S3 incomplete/uncertain upload details. FileWriteError
retains `operation` and original failure as cause. Python cancellation remains an
asyncio.CancelledError subclass (FileWriteCancelled), also retaining the operation.
TypeScript failures/cancellation use FileWriteError with the original AbortError
or statement failure in cause.

The operation remains caller-owned after statement success: outer transaction
failure, rollback, savepoint rollback or uncertain acknowledgement never triggers
physical deletion. No object ownership registry, deletion compensation, savepoint
tracking, cleanup outbox or lifecycle worker is implied. Applications explicitly
choose recovery and retention. Preparing/uploading then writing ordinary JSON
remains available for complete application control.

FileField's explicit signed URL/open operations distinguish omitted public fields
from loaded NULL, raising FileNotLoaded/MissingFile before provider access. Decode
and Reference access make no storage requests. FileField helper errors must map to
the selected ORM's public missing/unloaded error classes in final integration.

## Selected native model integration (current milestone)

The native `file-storage` Cargo feature includes the durable reference validator,
resolves logical JSON field positions at definition, and validates ordinary
insert/update/bulk-update references without provider calls. Both bindings expose
selected insert preflight that converts and plans all inputs without executing
SQL. No storage dependency or prepared file layout exists in disabled native core.

Selected code generation uses the fixed `file-storage.reference.v1` adapter:
Python imports `orm_file_storage.model.install_model`; TypeScript imports
`installModel` from `@orm/file-storage/model`. Generated modules expose
`configure_file_storage(registry)` / `configureFileStorage(registry)` for the
application's storage Registry. Methods delegate signing/opening explicitly;
reference-only writes normalize without uploading. Generated single insert and
unique-row update wrappers prepare Upload outside native SQL execution and retain
completed references after SQL failure. Unique updates require a direct equality
on a prepared primary-key/unique field, possibly within AND; arbitrary OR, bulk,
conflict and expression uploads reject before I/O.

Python retains the prepared operation on the selected statement's `operation`.
TypeScript's selected query exposes `prepareFileInsert(values)` returning
`{ operation, execute }` when the caller needs recovery after a successful
statement followed by outer rollback. Convenience errors in both bindings retain
the operation. Retry explicitly with its `.execute()`; no upload replay or
physical cleanup is automatic.

Dynamic hosts use the same setup function and fixed decoder. For TypeScript pass
`meta` as the fourth installModel argument to install selected query statements;
Python install_model installs statements when passed an ORM model class. The
host must stage this setup on candidate models before publishing its definition.
Selected descriptor maps use Python field names / TypeScript camelCase names.

Automatic public Reference materialization still requires the plan 03-owned
materializer to install the fixed decoder from `orm_file_storage.decoder` /
`@orm/file-storage/decoder`. Until that integration is adopted, native reads
return the ordinary JSON wire dictionary/object despite the generated Reference
type declaration. This milestone is not release-ready. Plan 02's immutable
ADAPTERS/nativeAdapters must select/import these modules once at initialization;
this branch does not fabricate that selector or advertise finished packaging.
