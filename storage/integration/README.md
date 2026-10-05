# File-field binding preparation

`python/` and `typescript/` contain separately packaged selected binding helpers.
They depend on the independent storage package. They do not import ORM internals,
provider SDKs, or install a runtime native callback registry.

These helpers and the `../orm-extension/` schema compiler are implemented and tested
independently. Automatic model definition/code generation/decoding and ORM write
integration remain pending the verified host/packaging APIs. Installing these
helpers alone does not enable annotated file fields in an ORM artifact.

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
