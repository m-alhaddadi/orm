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
  id          Int     @id
  name        String?
  status      Status  @default(OLD)
  legacy_note String?
}
model ActiveUser {
  name   String         @client_default("client default")
  status Status(ACTIVE) @client_default(ACTIVE)

  @@proxy.of(User)
  @@proxy.fields(exclude: ["legacy_note"])
}
```

A proxy inherits its source fields and relations, also through proxy chains.
It retains the root table, physical primary key, column types, enum representation,
constraints and database defaults. It creates no table or index. Physical foreign
keys that target a proxy refer to its root storage owner; logical relation loads
still return the proxy model. Cycles, unknown sources, stored-field additions,
encoding changes, key changes and physical object overrides fail during definition.
A proxy cannot override a relation; inherited relations keep their targets.
A proxy cannot declare `@@protected_write`; it shares the protection of its root model.

**Inherited fields.** `@@proxy.fields(include: [...])` or
`@@proxy.fields(exclude: [...])` selects the source fields and relations by name.
With no `@@proxy.fields`, the proxy inherits all of them. `include` and `exclude`
together is an error. With `include`, a relation that is not in the list is omitted.
An omitted field is not part of the proxy model: not in its class, its types, its
reads or its writes. An insert through the proxy leaves the column to its database
default. This is different from `@query.selectOut` and `@@query.defaults(fields:)`,
which only change the default selection and keep the field.

Definition fails with the field name when an omission breaks a rule:

* The primary key cannot be omitted.
* A `NOT NULL` field without a database `@default` cannot be omitted, because an
  insert through the proxy then fails. A `@client_default` on the source field does
  not count: the proxy insert does not contain the field.
* A field that a relation of the proxy uses as its key cannot be omitted, unless the
  relation is omitted too. A field that a relation of another model references on
  the proxy cannot be omitted.
* A child proxy selects from the fields of its source proxy, not from the root model.

An omitted nullable field also drops its `@client_default`: a proxy insert stores NULL, and a parent insert stores the default.
A proxy inherits `@@query.defaults` of its source; when it names an omitted field, definition fails with `model P has no field "note"`.
Override it on the proxy with a field list without the omitted field.

**Redeclared fields** change only the logical view. A redeclared field must be in
the inherited set, and it must equal the source field in everything except
nullability, the enum subset and `@client_default`. It can leave out `@default`;
it keeps the database default, because a proxy cannot change it.

* Nullability: `name String` declares a non-null view of a nullable field. Broader
  nullability never relaxes the physical database constraint.
* Enum subset: `Status(ACTIVE, OLD)` lists parent enum member names. Only a proxy
  field takes this type argument. It keeps the full parent runtime enum, so a query
  can still return a value outside the subset as its ordinary parent enum member.
* Client default: `@client_default(...)` (see [the schema](schema.md#client-defaults))
  replaces the inherited client default. A child proxy inherits the client defaults
  of its source, and a child redeclaration replaces the inherited one. An enum
  default uses member names (`ACTIVE`), and it must be in the field's enum subset. Defaults fill omitted insert values before
  native write transforms and validators. An explicit value, also SQL NULL, wins.

The physical server default remains available to writes through the parent and
other database clients. Filters never synthesize inserted values.

A narrowed declaration is a warning contract. It adds no filter, discards no row,
and rejects no write value that storage accepts. Nulls and enum subset violations
keep their actual decoded values. Counts, ordering and pagination follow SQL exactly.
A physical decoding error remains a decoding error.

Native diagnostics emit JSON to stderr once per operation/model/field/category:

```json
{"code":"orm.proxy.shape","model":"ActiveUser","field":"status","expected_shape":{"non_null":false,"enum_members":["ACTIVE"]},"category":"enum_subset","occurrence_count":2}
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
