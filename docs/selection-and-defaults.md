# Partial models and schema query defaults

`select()` retains its projection-row/scalar behavior. `only()` returns model
instances with an explicit public scalar-field selection:

```python
row = await User.objects.only(User.name, User.note).get(User.id == 1)
row.name                     # loaded value
row.note                     # None is a loaded SQL NULL
row.bio                      # raises orm.NotLoaded
row.pk                       # identity remains available internally
row.to_dict()                # public loaded scalar fields only
await row.refresh()          # preserves the public selection
await row.refresh(User.bio)   # expands it with an explicit field
await row.update(name="new") # preserves the public selection
```

TypeScript uses the same `only(User.name, User.note)` syntax, `NotLoaded`,
`toJSON()`, and `refresh(...columns)`. Explicit refresh columns add to the
current projection. Its partial result is `Partial<ModelRow> & Instance<ModelSpec>`;
methods and `pk` remain available.
Python returns the generated model class; field annotations describe a loaded
value, and omitted field access raises `NotLoaded` rather than returning `None`.
`only()` without columns resets to every public scalar field. It does not change
filters or reference loading. Column expressions and writes remain usable for
excluded fields. Insert requirements remain unchanged.

The optional `query-defaults` native capability and compiler crate
`orm-query-defaults` support schema policies:

```prisma
model User {
  id Int @id @default(autoincrement())
  name String
  active Boolean @default(true)
  bio String @query.selectOut
  @@query.defaults(filter: "active == true", fields: ["id", "name"], related: [])
}
```

Build-select the compiler as a dependency with alias `query_defaults`; its
manifest selects the `query-defaults` host capability. Disabled artifacts reject
policy metadata during definition with a rebuild instruction. Ordinary explicit
partial selections remain available in baseline builds.

Options omitted on a child inherit the nearest parent's policy. An explicit filter
replaces the inherited filter. `parent.default_filter and active == true` composes
it deliberately. A standalone policy may specify `parent: "Base"`; proxy and
composition declarations supply their parent. `filter: "none"` clears the filter,
`fields: ["*"]` resets to all fields, and `related: []` clears default loading.
An explicit field list replaces the inherited list; local `@query.selectOut`
removes that field from the default list. It retains the field in the model.
An empty selected list exposes no scalar fields, while retaining identity helpers.

Filter expressions support root fields, string/integer/boolean literals, NULL,
`== != < <= > >=`, parentheses, `and`/`or` (or `&&`/`||`), and `not`/`!`.
`not` binds looser than comparisons: `not a == 1` means `not (a == 1)`.
Literal types are checked by the database when the query runs.
`parent.default_filter` requires an inherited filter. Unknown fields, inheritance
cycles and recursive default loading reject during compilation/definition.
Related paths use schema names separated by dots and must be to-one references.

Python `without_defaults()` and TypeScript `withoutDefaults()` bypass default
filters, selections and eager loading, including `select_related` and
`prefetch_related` targets, while preserving caller filters and explicit
loading/selection. `without_related()` / `withoutRelated()` clears default and
current explicit eager loading; subsequent explicit loading adds references.
Defaults affect reads, count/exists, and queryset update/delete selection, including
bulk updates. Inserts use declared client/database defaults and caller values;
filters never provide inserted values.

A filtered-out joined target is `None`/`null`, including a physically required
reference. Its default filter is in the LEFT JOIN condition, so the source
survives. Without a target default filter, a set key with no joined row still
raises `NotLoaded`.
Generated reference types become nullable for filtered targets. This is absence in
the filtered view, not evidence that the physical row is absent.

UPDATE/DELETE `returning` uses the query's scalar selection, including schema
defaults. It returns the actual written row even when the mutation leaves the
filter. Instance update/delete/refresh use internal identity and bypass defaults;
instance update and refresh fetch only the instance's public fields and helpers.
Insert and bulk-update returning retain their existing complete-row result contract.

Internally, `ResultShape` maps logical `FieldId`s to actual row slots and marks
public versus helper fields. PK, reference keys and computation dependencies can
be fetched privately. `Select.model_helpers` requests additional private columns.
Whole-model shapes, including `only()` without columns, retain the original
materialization fast path. Binding loaders
use Python `_field_value(name)` or TypeScript `fieldValue(row,name)` to read helper
keys; neither operation changes public loaded state.
