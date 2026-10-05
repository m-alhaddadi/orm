# Same-table proxy models

## Outcome

Schema-defined proxies expose different default shapes and behavior over one
physical model/table. Proxy chains support field selection, narrowed attributes,
default filters, and new field defaults. Implement this as a statically selected
extension using plans 1 and 3.

## Schema and storage

Each proxy names one source model or proxy. Resolve chains and reject cycles.
Record logical model identity separately from physical table identity, physical
field types, and constraints. A proxy creates no table, indexes, or narrowed physical
constraints; migrations derive physical objects from storage owners once.

Allowed initial overrides:

- Excluding a field from default selection, with explicit loading still available.
- Declaring a non-null view of a nullable field or an enum subset.
- Replacing schema-defined client insert defaults.
- Default filters and selection/loading declarations from plan 3.

Reject overrides that require a different storage type, incompatible encoding,
changed PK identity, or database constraint changes. Broadening a field declaration
does not relax physical constraints. Adding custom validators is a separate feature.

## Reads and diagnostics

Compile the user's declared filter exactly, including explicit parent references.
Do not infer a non-null predicate or enum predicate from a narrowed declaration.

When a loaded value violates the proxy shape, return it and emit a structured
warning naming model, field, expected shape, violation category, and occurrence count.
Aggregate by query/model/field/category to avoid per-row log spam. Do not log raw
field values or add extra queries to inspect unselected fields. Null and enum subset
checks use ordinary compiled decoding code rather than extension callbacks.

Subset enums retain the parent's runtime representation so out-of-subset values
can still be returned. Generated types describe the intended subset; document that
warning-only contracts can violate those hints. Failure to decode the underlying
physical type remains an ordinary decoding error, not a proxy contract warning.

Counts, pagination, ordering, and result rows remain consistent because no rows are
discarded after SQL execution. Diagnostics cover selected model fields, not unrelated
scalar projections or count queries. Contract checks have intrinsic work; the
extension mechanism adds no dispatch overhead.

## Writes

Insert/update/delete are supported through the proxy. Client defaults are compiled
from its schema declaration; a proxy default does not alter the physical server
default used by other writers. Explicit values override defaults. A default filter
does not synthesize write values.

Physical database constraints still apply. Narrowed-shape violations warn and retain
actual values rather than rejecting a value that the storage model accepts. Follow
plan 3 for update/delete selection, bypassing defaults, and rows moving outside a
filter after updates.

## Tickets and acceptance

1. Extend schema declarations/IR through the extension and validate chains/overrides.
2. Separate logical proxy metadata from physical schema/migration ownership.
3. Generate equivalent Python/TypeScript models and narrowed types.
4. Add warning-only decoding and proxy write defaults.
5. Verify query and migration behavior across PostgreSQL/SQLite.

Cover incorrect filters, null/enum violations, log aggregation, explicit hidden
field loading, insert defaults, updates escaping a filter, relation targets pointing
at proxies, and an unchanged physical migration for proxy-only edits.
