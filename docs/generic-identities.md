# Frozen model identities

`orm identities` explicitly generates `schema.identities.json` beside the root
schema. It collects the root and its explicit `.prisma` imports, preserving table
prefixes and the separate extension catalog import mechanism. It never scans
neighboring schemas. Commit the manifest with the schema.

Initial positive signed 32-bit IDs are allocated in model-name order. Subsequent
models receive IDs above the highest allocation; existing values never move.
Removed models retain permanent retired entries. Reintroducing a name allocates a
new ID unless `orm identities --restore Model` explicitly restores its single
retired identity; the entry's `restorations` count records that intent, and a
manifest that reactivates a tombstone without it is rejected. Renames use
`orm identities --rename Old=New`. Rename chains,
including a return to an original name, retain the original ID and history.
The command prints each added, renamed, retired, and restored entry.
Moving a declaration between files or changing its table mapping preserves its ID.

Compilation, generation, runtime definition and migration planning only read
identities. Missing/stale mappings fail with a generation instruction. Compilation
synthesizes one ordinary integer-storage `ContentType` enum, ordered by ID; Python
uses IntEnum and TypeScript named integer members. Its values agree in generated
bindings and runtime-loaded models. Python obtains runtime members from
`registry.get_enum("ContentType")`; TypeScript uses `registry.getEnum("ContentType")`.
Inline `define()` consumes compiled IR with embedded `identities` and `enums`.
No database metadata table or registration rows are created.

Identity-bearing migration snapshots use version 3. They retain all retired IDs
and reject removed/reassigned historical allocations. Identity-only changes still
produce a reviewable snapshot migration. Ordinary integer enum constraints prevent
unknown/retired values in referencing columns; a retirement migration fails if
existing rows still contain the removed discriminator. Clean up those references
explicitly before retrying. Physical model removal is ordinary migration behavior.

Concrete names and generated Python/TypeScript names must be globally unambiguous.
Physical tables cannot collide under database identifier rules. Logical proxies
are excluded using the typed proxy setup metadata and resolve to their concrete
storage owner; they do not receive independent identities.

An ordinary handwritten enum called ContentType remains an ordinary enum when no
identity manifest/generic relation requests synthesis. Declaring an enum never
enables generic relation behavior by itself.

## Generic relation compiler contribution

The selected `orm-generic` compiler crate declares these namespaced attributes:

```prisma
model Tag {
  id           Int @id
  content_type ContentType?
  object_id    Int?

  @@generic.relation("target", type: "content_type", key: "object_id", targets: ["Post", "Photo"])
  @@index([content_type, object_id])
}

model Post {
  id Int @id
  @@generic.reverse("tags", source: "Tag", relation: "target")
}
```

The compiler validates allowed concrete targets, compatible single-column key
encodings and matching pair nullability. It contributes ordinary check constraints
for paired nulls and the allowed discriminator subset, plus forward/reverse setup
metadata. It contributes no conventional multi-target foreign key. Database checks
preserve pair validity for external scalar writes without promising target existence.

**Delivery state:** this milestone implements identities and compiler contributions.
The native generic routing/write/loading/prefetch/reverse APIs and binding unions
are still being implemented. The compiler crate alone does not supply them. Native
artifacts without the generic runtime capability reject generic setup metadata at
definition. No capability is advertised on the basis of this compiler scaffolding.
