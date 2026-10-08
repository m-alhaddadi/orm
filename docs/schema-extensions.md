# Column-role extensions: `updated_at`, soft delete, optimistic locking

Three build-time extensions give a column a role that the ORM keeps on writes.
Each one is a crate in `extensions/` with a host capability and Cargo feature of the same name.
Select the crate in the `orm-extension-build` configuration (see `build-time-extensions.md`); do not set the feature by hand.

| Crate | Capability | Attribute |
|---|---|---|
| `orm-timestamps` (`extensions/timestamps`) | `updated-at` | `@timestamps.updated_at(mode:)` |
| `orm-soft-delete` (`extensions/soft-delete`) | `soft-delete` | `@soft_delete.deleted_at(mode:)` |
| `orm-locking` (`extensions/locking`) | `optimistic-locking` | `@locking.version` |

You declare the column; the attribute gives it the role.
A build without the extension has no engine code for the role, and a schema with its attribute fails with "rebuild".

## Modes

`@timestamps.updated_at` and `@soft_delete.deleted_at` take `mode: "database"` (the default) or `mode: "application"`.

* Database mode adds row triggers to the migration, so every writer gets the behavior, also raw SQL.
* Application mode adds no DDL. Only ORM writes get the behavior.

ORM writes are the same in both modes.
The ORM sets the column in the statement, so `RETURNING` gives the new value on Postgres and SQLite.

## `updated_at`

```prisma
model Note {
  id         Int      @id
  title      String
  updated_at DateTime @default(now()) @timestamps.updated_at
}
```

* Each ORM update (query set `update()`, instance `update()`, `update_many()`) sets the field to the current time, unless the update sets the field.
* An insert uses the field default; give `@default(now())`.
* Database mode, Postgres: a `BEFORE UPDATE ... FOR EACH ROW WHEN (NEW.updated_at IS NOT DISTINCT FROM OLD.updated_at)` trigger sets `NEW.updated_at := now()`.
  An update that writes the field keeps its value.
* Database mode, SQLite: SQLite cannot assign `NEW` in a trigger.
  An `AFTER UPDATE ... WHEN NEW.updated_at IS OLD.updated_at` trigger updates the row to `CURRENT_TIMESTAMP`.
  SQLite `RETURNING` does not show a change made by an `AFTER` trigger; ORM updates set the field themselves, so the trigger does not run for them.

## Soft delete

The API follows django-safedelete.

```prisma
model Author {
  id         Int       @id
  deleted_at DateTime? @soft_delete.deleted_at
  books      Book[]
  @@query.filter("deleted_at == null")   // optional: hide deleted rows (orm-query-defaults)
}
model Book {
  id         Int       @id
  author_id  Int
  author     Author    @relation(fields: [author_id], references: [id], onDelete: Cascade)
  deleted_at DateTime? @soft_delete.deleted_at
}
```

* The field is a nullable `DateTime`. One field for each model.
* `delete()` on a query set or an instance sends `UPDATE ... SET deleted_at = <now> WHERE ... AND deleted_at IS NULL`, never `DELETE`.
  The count and `RETURNING` are the rows that the call soft-deleted; rows that are already soft-deleted do not count.
* `hard_delete()` (`hardDelete()`) sends `DELETE`.
  In database mode, the trigger soft-deletes a live row, so `hard_delete()` deletes only rows that are already soft-deleted.
* `undelete()` on a query set clears the field of the matching soft-deleted rows; on an instance, it clears the field of its row.
* `all_with_deleted()` (`allWithDeleted()`) is `without_defaults()`: it removes the schema default filter that hides deleted rows.
  `deleted_only()` (`deletedOnly()`) gives only the soft-deleted rows.
* Reads show soft-deleted rows unless the model has a default filter.
  Select `orm-query-defaults` and add `@@query.filter("deleted_at == null")`.
  `without_defaults()` removes every schema default, not only this filter.
* Database mode adds a `BEFORE DELETE ... FOR EACH ROW WHEN (OLD.deleted_at IS NULL)` trigger.
  It sets the field and cancels the delete (Postgres `RETURN NULL`, SQLite `RAISE(IGNORE)`).
  A raw `DELETE` of a live row affects 0 rows; a second `DELETE` deletes the soft-deleted row.
* Cascade, database mode: a cancelled delete does not fire `ON DELETE CASCADE`.
  For each soft-delete child with `onDelete: Cascade`, the parent gets an `AFTER UPDATE OF deleted_at` trigger.
  When the parent row becomes soft-deleted, the trigger sets the same time on the live child rows.
  A child without soft delete keeps its rows.
* Application mode has no cascade.
* A composed model (`model-composition`) keeps its own delete plan.

## Optimistic locking

```prisma
model Doc {
  id      Int    @id
  title   String
  version Int    @default(0) @locking.version
}
```

* The field is a required `Int` or `BigInt`. One field for each model.
* Each ORM update (query set `update()`, instance `update()`, `update_many()`, a soft delete) adds `version = version + 1`, unless the update sets the field.
* An instance `update()` or `delete()` also matches the loaded version: `WHERE pk = ... AND version = <loaded>`.
  When no row matches and the row has another version, it raises `orm.VersionConflict` (`VersionConflict` in TypeScript).
  When the row is gone, `update()` raises `DoesNotExist`.
* The instance must have the version field loaded.
* A query set `update()` does not check the version, as Rails `update_all`.
* No DDL.

```python
doc = await Doc.objects.get(Doc.id == 1)
try:
    await doc.update(title="new")
except orm.VersionConflict:
    await doc.refresh()          # load the other writer's change, then decide
```
