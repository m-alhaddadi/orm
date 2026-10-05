# Composition with shared primary keys

## Outcome

A schema-defined child stores its own columns in a separate table and links to
one parent using a shared primary key. A parent can simultaneously have different
child types. Querying a parent returns that parent's model type.

The schema may expose parent fields on a child's logical shape for convenient
querying and writes. This is storage/query composition, without automatic runtime
subclass conversion or exclusive sibling types. Chains are supported; multiple
composed parents for one child are outside the initial scope.

## Physical and logical schema

- The child PK is also an FK to its parent PK, with the same storage type.
- Parent fields remain stored on their declaring table; children store local fields.
- Child PKs do not independently auto-increment.
- Generate ordinary child-to-parent and parent-to-child one-to-one relations.
- Allow a person row to have both employee and customer rows.
- Track table ownership for every flattened field; qualify inherited column
  expressions with the correct join alias in filters, ordering, and selection.
- Reject cycles, ambiguous field names, and unsupported field overrides.

Keep physical migration IR separate from flattened logical model fields so parent
columns are never duplicated into a child's migration. Initial child creation is
additive; reparenting populated tables and migrating independent identities into
shared PKs require separate migration work.

## Reads and relation loading

Child queries join the required ancestor tables and decode one composed child shape.
Parent queries do not automatically join every child table. Named references use
normal optional one-to-one semantics:

```python
people = await Person.objects.select_related(Person.employee, Person.customer)
# person.employee and person.customer are independently loaded objects or None.
```

Default eager loading can be declared in the schema. Explicit loaders from plan 4
are available when a reference was not loaded. No `child`, `load_child`, or
`cast_down` property/method is generated. Applications can define an ordinary
convenience property choosing among loaded references if they need one.

Defaults targeting child fields remain scoped to child joins; they must not filter
out parent rows. A named loaded reference holds the object returned by its explicit
loader, preserving identity within that instance cache.

## Atomic writes

Engine-managed operations are the portable first implementation:

1. Creating a complete child splits values by owning table, inserts ancestors from
   root to leaf, and propagates the shared PK in one transaction.
2. Attaching a child to an existing parent uses an explicit operation with local
   child values. It must not accidentally overwrite existing parent values.
3. Updating flattened parent/local values executes all affected table writes in one
   transaction; a failure rolls back every write.
4. Bulk writes operate on one consistent matched identity set; do not independently
   re-evaluate filters after changing parent fields. Use the transaction/locking
   strategy appropriate to each database and report affected logical object counts.
5. Write-returning assembles the final logical shape from actual stored values.

Defaults come from schema declarations, never inferred from query filters. Existing
transactions remain authoritative; nested composed operations must not commit an
outer transaction. Protect attach/create races with PK/FK constraints and normal
transaction semantics, without inventing sibling exclusivity constraints.

## Deletion

Deleting a child removes that child and descendants depending on it. It preserves
ancestors and other sibling branches. Deleting a parent cascades to all dependent
children through ordinary FKs. For a chain `Person -> Employee -> Manager`, deleting
Employee also removes Manager, while Person and its Customer row survive.

Reverse deletion of a referenced parent is the separate `owningFk` feature. It is
not a prerequisite and must address shared references before it can be enabled.

## Triggers and rules

Defer automatic parent insertion through triggers/rules. A child-table insert does
not carry inherited parent columns; a writable joined view or another insert
contract is needed. A later backend extension can evaluate that design, generated
keys, returning behavior, defaults, and interoperability with direct SQL writers.
Do not ship different composition semantics per database as a shortcut.

## Tickets and acceptance

1. Add extension schema/IR and shared-PK migrations with ordinary generated relations.
2. Implement field ownership, ancestor joins, and composed result decoding.
3. Implement atomic create and explicit attach-to-existing-parent operations.
4. Implement atomic updates, consistent bulk identities, and returning behavior.
5. Verify deletion, reference loading, generated types, and database parity.

Cover simultaneous Employee/Customer records, longer chains, attach races, rollback
at every table operation, generated and explicit keys, filtered parent/child views,
default selection, counts, child-only deletion, and parent cascading. Deliver
Python and TypeScript behavior with each supported PostgreSQL/SQLite slice.
