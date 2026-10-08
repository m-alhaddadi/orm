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

Each option also has its own attribute.
One option comes from one place on a model:

```prisma
model Base {
  id Int @id @default(autoincrement())
  @@query.filter("id > 0")
}
model Profile {
  id Int @id @default(autoincrement())
  user User?
}
model User {
  id Int @id @default(autoincrement())
  name String
  active Boolean @default(true)
  created_at DateTime @default(now())
  bio String @query.selectOut
  profile_id Int @unique
  profile Profile @relation(fields: [profile_id], references: [id])
  @@query.filter("active == true")
  @@query.fields(["id", "name"])
  @@query.related(["profile"])
  @@query.order("-created_at", "id")
  @@query.parent("Base")
}
```

`@@query.defaults(filter: ...)` together with `@@query.filter(...)` is an error that names both.
A second copy of the same attribute is an error too.

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

Filter expressions support root fields, string/integer/boolean literals, `scope.<name>` values, NULL,
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

### Scope values: `scope.<name>`

A filter can read a value that the application sets for the current request or task:

```prisma
model Order {
  id      Int @id @default(autoincrement())
  shop_id Int
  @@query.filter("shop_id == scope.shop")
}
```

```python
with orm.scope(shop=shop.id):          # a sync `with`; tasks started inside get it
    orders = await Order.objects.filter(Order.total > 10)
```

```ts
await scope({ shop: shop.id }, async () => { const orders = await Order.objects.filter(...); });
```

* The frontend sends the scope values as parameters of each statement; the value is bound with the type of the column it is compared with.
* Closed by default: a read, count, exists, update, delete or `update_many` on a model whose default filter reads `scope.shop` raises `QueryError` when no enclosing `scope()` sets `shop`. Relation hops, prefetches and joined targets of that model check it too.
* `without_defaults()` / `withoutDefaults()` skips the filter and the check.
* Inner `scope()` values replace outer ones.
* Inserts do not read the scope: set `shop_id` yourself.
* An awaited query set keeps its rows; awaiting it again under another scope gives the first rows.
* For a check in the database as well, use Postgres row-level security with `db.tenant(id)` (see the API docs).

The default order is one string for each order column: a field, `-` before it for descending, and an optional ` nulls first` or ` nulls last`.
`@@query.defaults` takes the same strings as a list: `order: ["-created_at", "id"]`.
Order columns are fields of the model itself, not related columns.
A child inherits the order, and `@@query.order()` or `order: []` clears it.
The schema load checks the resolved order of each model, inherited ones included: each column is a field of the model, once; a nullable column has `nulls first` or `nulls last`; and a `json` or `xml` column is an error.
Index the order columns: without an index, each read sorts all matching rows.
A composed child that inherits the order reads the parent's columns through a subquery, which no index serves.

The default order applies to model reads that give no `order_by()` / `orderBy()`, which include `first()`, `last()` and the rows of a `Prefetch` query set without its own order.
An explicit order replaces it, and `without_defaults()` removes it.
`select(...)` rows, subqueries, `count`, `exists`, `update` and `delete` ignore it.
For a composed model, the internal reads of an insert, update or delete keep it; it does not change which rows they write.
`batches()` and `iterate()` keep their primary-key order.

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
