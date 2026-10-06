# Same-table proxy models

This feature is implemented in `orm-proxy` (schema compiler) and the optional
`proxy-models` native capability. Final selection/default integration with plan 03
is still pending; this guide describes the proxy declarations and verified core
behavior, not a completed release.

```prisma
enum Status {
  ACTIVE @map("active")
  OLD @map("old")
  @@storage(text)
}
model User {
  id Int @id
  name String?
  status Status @default(OLD)
}
model ActiveUser {
  @@proxy.of(User)
  @@proxy.nonNull("name")
  @@proxy.subset("status", [ACTIVE])
  @@proxy.default("name", "client default")
  @@proxy.default("status", "active")
}
```

A proxy inherits its source fields and relations, including through proxy chains.
It retains the root table, physical primary key, column types, enum representation,
constraints and database defaults. It creates no table or index. Physical foreign
keys targeting a proxy refer to its root storage owner; logical relation loads
still return the proxy model. Cycles, unknown sources, stored-field additions,
encoding changes, key changes and physical object overrides fail during definition.
Proxy relation overrides are currently unsupported; inherited targets are preserved.

`proxy.nonNull` changes the intended view of a nullable field. `proxy.subset` lists
parent enum member names. It preserves the full parent runtime enum, so a value
outside the subset can still be returned as its ordinary parent enum member.
Repeated compatible field declarations may broaden logical nullability; they never
relax the physical database constraints. Namespaced declarations avoid repeating
storage definitions.

`proxy.default` replaces a client insert default. Literal strings, numbers,
booleans, null, arrays and JSON objects are supported when compatible with the
physical field encoding. Enum defaults use the stored parent enum value (the
example uses `"active"`); subset declarations use member names (`ACTIVE`). Defaults
are parsed once during definition, inherited through chains, and replaced by child
defaults for the same field. They fill omitted insert values before native write
transforms and validators. Explicit supplied values, including SQL NULL, win.
The physical server default remains available to writes through the parent and
other database clients. Filters never synthesize inserted values.

Narrowing is a warning contract. It adds no filter, discards no row, and rejects no
write value accepted by storage. Nulls and enum subset violations retain their
actual decoded values. Counts, ordering and pagination follow SQL exactly. A
physical decoding error remains a decoding error.

Native diagnostics emit JSON to stderr once per operation/model/field/category:

```json
{"code":"orm.proxy.shape","model":"ActiveUser","field":"status","expected_shape":{"non_null":true,"enum_members":["ACTIVE"]},"category":"enum_subset","occurrence_count":2}
```

The records contain schema metadata and counts, never raw field values. Checks
apply to publicly loaded instance fields, including related and returning shapes;
unloaded fields are not inspected. Count and scalar projection results do not
produce proxy warnings. A missing LEFT JOIN object is absence, not a null-shape
violation. The pending plan 03 integration must supply partial public shapes at
the operation boundary before this feature is ready for integration.

Generated Python and TypeScript types describe the intended non-null/subset view.
Because these contracts warn rather than enforce, returned values can violate
those hints. Strict validation requires a separately declared validator.

Feature-disabled native builds exclude prepared proxy state, runtime dependencies
and diagnostic/default execution. Definition rejects proxy metadata without the
compiled capability and asks for a rebuild. The compiler is selected through the
build-time extension manifest; enabling runtime code alone does not install its
source declarations.
