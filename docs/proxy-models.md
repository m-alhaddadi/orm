# Same-table proxy models

The `orm-proxy` schema compiler and the optional `proxy-models` native capability
supply proxy models. A proxy is a different logical view of one physical model.
Default filters and default selection come from plan 03 (selection and defaults).

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
model ActiveUser {
  name String @default("name", "client default")
  status [ACTIVE] @default(ACTIVE)

  @@proxy.of(User)
}

```

A proxy inherits its source fields and relations, also through proxy chains.
It retains the root table, physical primary key, column types, enum representation,
constraints and database defaults. It creates no table or index. Physical foreign
keys that target a proxy refer to its root storage owner; logical relation loads
still return the proxy model. Cycles, unknown sources, stored-field additions,
encoding changes, key changes and physical object overrides fail during definition.
A proxy cannot override a relation; inherited relations keep their targets.

`proxy.nonNull` changes the intended view of a nullable field. `proxy.subset` lists
parent enum member names. It keeps the full parent runtime enum, so a query can
still return a value outside the subset as its ordinary parent enum member.
Repeated compatible field declarations may broaden logical nullability; they never
relax the physical database constraints. Namespaced declarations do not repeat
storage definitions.

`proxy.default` replaces a client insert default. A default can be a literal
string, number, boolean, null, array or JSON object that the physical field encoding
accepts. An enum default uses the stored parent enum value (the example uses
`"active"`); a subset declaration uses member names (`ACTIVE`). Definition parses
each default once. A child proxy inherits the defaults of its source, and a child
default for the same field replaces the inherited one. Defaults fill omitted insert
values before native write transforms and validators. An explicit value, also SQL
NULL, wins over a default.
The physical server default remains available to writes through the parent and
other database clients. Filters never synthesize inserted values.

A narrowed declaration is a warning contract. It adds no filter, discards no row,
and rejects no write value that storage accepts. Nulls and enum subset violations
keep their actual decoded values. Counts, ordering and pagination follow SQL exactly.
A physical decoding error remains a decoding error.

Native diagnostics emit JSON to stderr once per operation/model/field/category:

```json
{"code":"orm.proxy.shape","model":"ActiveUser","field":"status","expected_shape":{"non_null":true,"enum_members":["ACTIVE"]},"category":"enum_subset","occurrence_count":2}
```

The records contain schema metadata and counts, never raw field values. The checks
apply to publicly loaded instance fields, also related and returning shapes. The
checks do not inspect unloaded fields. Count and scalar projection results give no
proxy warnings. A missing LEFT JOIN object is an absent object, not a null-shape
violation. Partial public shapes at the operation boundary come from plan 03.

Generated Python and TypeScript types describe the intended non-null/subset view.
These contracts give warnings and do not enforce the shape, thus returned values
can violate those hints. Strict validation requires a separately declared validator.

A native build without the feature excludes prepared proxy state, runtime
dependencies and diagnostics. Definition rejects proxy metadata without the
compiled capability and asks for a rebuild. The `orm-proxy` manifest declares the
`proxy-models` capability, so selecting it in `orm-extension-build` selects both the
compiler and the `proxy-models` host feature; the runtime code alone does not install
the source declarations.
