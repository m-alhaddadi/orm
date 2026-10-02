//! Schema diff: the operations that turn one [`DbSchema`] into another.
//!
//! Tables and columns are matched by name, or through rename hints. Indexes and
//! constraints are matched by name first and then by definition, so an object whose
//! generated name changed (because its table was renamed) is renamed, not rebuilt.
//! Anything else that changed is dropped and recreated; columns are altered in place.
//!
//! Operations come out in an order Postgres accepts: extensions and functions first,
//! then everything that depends on old objects is dropped, tables and columns change,
//! new constraints / indexes / triggers are created, and unused functions and
//! extensions go last. Foreign keys are created after every table they reference.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::model::{
    Check, Column, DbSchema, Exclusion, Extension, ForeignKey, Function, Index, PrimaryKey, Renames, Table, Trigger,
    Unique,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Constraint {
    PrimaryKey(PrimaryKey),
    Unique(Unique),
    Check(Check),
    Exclusion(Exclusion),
    ForeignKey(ForeignKey),
}

impl Constraint {
    pub fn name(&self) -> &str {
        match self {
            Constraint::PrimaryKey(c) => &c.name,
            Constraint::Unique(c) => &c.name,
            Constraint::Check(c) => &c.name,
            Constraint::Exclusion(c) => &c.name,
            Constraint::ForeignKey(c) => &c.name,
        }
    }

    fn renamed(&self, name: &str) -> Constraint {
        let mut c = self.clone();
        match &mut c {
            Constraint::PrimaryKey(c) => c.name = name.into(),
            Constraint::Unique(c) => c.name = name.into(),
            Constraint::Check(c) => c.name = name.into(),
            Constraint::Exclusion(c) => c.name = name.into(),
            Constraint::ForeignKey(c) => c.name = name.into(),
        }
        c
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    CreateExtension(Extension),
    DropExtension(Extension),
    /// `CREATE OR REPLACE FUNCTION`
    CreateFunction(Function),
    DropFunction(Function),
    /// Columns, primary key, unique / check / exclusion constraints and the foreign keys
    /// whose target exists by then. Indexes, triggers and comments are separate ops.
    CreateTable { table: Table, foreign_keys: Vec<ForeignKey> },
    DropTable { table: String },
    RenameTable { from: String, to: String },
    RenameColumn { table: String, from: String, to: String },
    AddColumn { table: String, column: Column },
    DropColumn { table: String, column: String },
    AlterColumnType { table: String, column: String, from: String, to: String },
    SetNotNull { table: String, column: String },
    DropNotNull { table: String, column: String },
    SetDefault { table: String, column: String, default: String },
    DropDefault { table: String, column: String },
    AddIdentity { table: String, column: String },
    DropIdentity { table: String, column: String },
    AddConstraint { table: String, constraint: Constraint },
    DropConstraint { table: String, name: String },
    RenameConstraint { table: String, from: String, to: String },
    CreateIndex { table: String, index: Index },
    DropIndex { name: String },
    RenameIndex { from: String, to: String },
    CreateTrigger { table: String, trigger: Trigger },
    DropTrigger { table: String, name: String },
    CommentOnTable { table: String, comment: Option<String> },
    CommentOnColumn { table: String, column: String, comment: Option<String> },
}

impl Op {
    /// Why the operation needs attention, if it can lose data or fail on existing rows.
    pub fn warning(&self) -> Option<String> {
        Some(match self {
            Op::DropTable { table } => format!("drops table {table} and all its rows"),
            Op::DropColumn { table, column } => format!("drops column {table}.{column} and its data"),
            Op::AlterColumnType { table, column, from, to } => {
                format!("changes {table}.{column} from {from} to {to}; existing values must convert")
            }
            Op::SetNotNull { table, column } => format!("fails if {table}.{column} holds NULLs"),
            Op::AddColumn { table, column }
                if !column.nullable && column.default.is_none() && !column.identity =>
            {
                format!("adds NOT NULL column {table}.{} without a default; fails if {table} has rows", column.name)
            }
            Op::AddConstraint { table, constraint } => match constraint {
                Constraint::Unique(c) => format!("{table}: unique {} fails if existing rows have duplicates", c.name),
                Constraint::Check(c) => format!("{table}: check {} fails if existing rows violate it", c.name),
                Constraint::Exclusion(c) => format!("{table}: exclusion {} fails if existing rows conflict", c.name),
                Constraint::ForeignKey(c) => format!("{table}: {} fails on rows without a matching key", c.name),
                Constraint::PrimaryKey(_) => return None,
            },
            Op::DropExtension(e) => format!("drops extension {}; other schemas may still use it", e.name),
            _ => return None,
        })
    }
}

/// `items` of `new` matched against `old`: by name, then by definition (a rename).
/// Returns (created, dropped, renamed (old, new)).
fn match_named<'a, T: PartialEq + Clone>(
    old: &'a [T],
    new: &'a [T],
    name: impl Fn(&T) -> &str,
    with_name: impl Fn(&T, &str) -> T,
) -> (Vec<&'a T>, Vec<&'a T>, Vec<(&'a T, &'a T)>) {
    let mut used_old: HashSet<usize> = HashSet::new();
    let mut created = vec![];
    let mut pending = vec![];
    for n in new {
        match old.iter().position(|o| name(o) == name(n)) {
            Some(i) => {
                used_old.insert(i);
                if old[i] != *n {
                    created.push(n); // changed: drop + create
                    pending.push(i);
                }
            }
            None => created.push(n),
        }
    }
    let mut dropped: Vec<&T> = pending.iter().map(|&i| &old[i]).collect();
    let mut renamed = vec![];
    created.retain(|n| {
        let candidate = old.iter().enumerate().find(|(i, o)| {
            !used_old.contains(i) && name(o) != name(n) && with_name(o, name(n)) == **n
        });
        match candidate {
            Some((i, o)) => {
                used_old.insert(i);
                renamed.push((o, *n));
                false
            }
            None => true,
        }
    });
    dropped.extend(old.iter().enumerate().filter(|(i, _)| !used_old.contains(i)).map(|(_, o)| o));
    (created, dropped, renamed)
}

fn constraints(t: &Table) -> Vec<Constraint> {
    let mut out = vec![];
    out.extend(t.primary_key.iter().cloned().map(Constraint::PrimaryKey));
    out.extend(t.uniques.iter().cloned().map(Constraint::Unique));
    out.extend(t.checks.iter().cloned().map(Constraint::Check));
    out.extend(t.exclusions.iter().cloned().map(Constraint::Exclusion));
    out
}

/// `t` as it looks after the table and column renames in `renames`, so it can be
/// compared with the new definition.
fn apply_renames(t: &Table, table_map: &HashMap<&str, &str>, col_map: &HashMap<(&str, &str), &str>) -> Table {
    let new_table = table_map.get(t.name.as_str()).copied().unwrap_or(&t.name).to_owned();
    let col = |table: &str, c: &str| -> String { col_map.get(&(table, c)).copied().unwrap_or(c).to_owned() };
    let cols = |table: &str, cs: &[String]| -> Vec<String> { cs.iter().map(|c| col(table, c)).collect() };
    let mut out = t.clone();
    out.name = new_table;
    for c in &mut out.columns {
        c.name = col(&t.name, &c.name);
    }
    if let Some(pk) = &mut out.primary_key {
        pk.columns = cols(&t.name, &pk.columns);
    }
    for u in &mut out.uniques {
        u.columns = cols(&t.name, &u.columns);
    }
    for ix in &mut out.indexes {
        for k in &mut ix.keys {
            k.column = k.column.as_ref().map(|c| col(&t.name, c));
        }
        ix.include = cols(&t.name, &ix.include);
    }
    for ex in &mut out.exclusions {
        for e in &mut ex.elements {
            e.key.column = e.key.column.as_ref().map(|c| col(&t.name, c));
        }
    }
    for fk in &mut out.foreign_keys {
        fk.columns = cols(&t.name, &fk.columns);
        fk.ref_columns = cols(&fk.ref_table, &fk.ref_columns);
        fk.ref_table = table_map.get(fk.ref_table.as_str()).copied().unwrap_or(&fk.ref_table).to_owned();
    }
    for tr in &mut out.triggers {
        tr.update_of = cols(&t.name, &tr.update_of);
    }
    out
}

/// Tables in an order where every foreign key target comes first; foreign keys that
/// close a cycle are returned separately (created later with `ALTER TABLE`).
fn creation_order<'a>(tables: &[&'a Table]) -> (Vec<&'a Table>, Vec<(String, ForeignKey)>) {
    let by_name: HashMap<&str, usize> = tables.iter().enumerate().map(|(i, t)| (t.name.as_str(), i)).collect();
    let mut state = vec![0u8; tables.len()];
    let mut order = vec![];
    let mut deferred = vec![];
    fn visit(
        i: usize,
        tables: &[&Table],
        by_name: &HashMap<&str, usize>,
        state: &mut [u8],
        order: &mut Vec<usize>,
        deferred: &mut Vec<(String, ForeignKey)>,
    ) {
        state[i] = 1;
        for fk in &tables[i].foreign_keys {
            if let Some(&t) = by_name.get(fk.ref_table.as_str()) {
                if t == i {
                    continue;
                }
                match state[t] {
                    0 => visit(t, tables, by_name, state, order, deferred),
                    1 => deferred.push((tables[i].name.clone(), fk.clone())),
                    _ => {}
                }
            }
        }
        state[i] = 2;
        order.push(i);
    }
    for i in 0..tables.len() {
        if state[i] == 0 {
            visit(i, tables, &by_name, &mut state, &mut order, &mut deferred);
        }
    }
    (order.into_iter().map(|i| tables[i]).collect(), deferred)
}

/// Operations turning `old` into `new`. `renames` maps new names to old ones.
pub fn diff(old: &DbSchema, new: &DbSchema, renames: &Renames) -> Vec<Op> {
    let mut ops = vec![];

    // -- extensions and functions --------------------------------------------------------
    for e in &new.extensions {
        if !old.extensions.iter().any(|o| o.name == e.name) {
            ops.push(Op::CreateExtension(e.clone()));
        }
    }
    let old_fns: HashMap<_, &Function> = old.functions.iter().map(|f| (f.key(), f)).collect();
    let new_fns: HashMap<_, &Function> = new.functions.iter().map(|f| (f.key(), f)).collect();
    for f in &new.functions {
        if old_fns.get(&f.key()) != Some(&f) {
            ops.push(Op::CreateFunction(f.clone()));
        }
    }

    // -- match tables --------------------------------------------------------------------
    let old_by_name: HashMap<&str, &Table> = old.tables.iter().map(|t| (t.name.as_str(), t)).collect();
    let new_names: HashSet<&str> = new.tables.iter().map(|t| t.name.as_str()).collect();
    let mut pairs: Vec<(&Table, &Table)> = vec![]; // (old, new)
    let mut created: Vec<&Table> = vec![];
    let mut table_map: HashMap<&str, &str> = HashMap::new(); // old -> new
    for n in &new.tables {
        if let Some(o) = old_by_name.get(n.name.as_str()) {
            pairs.push((o, n));
            table_map.insert(&o.name, &n.name);
            continue;
        }
        let renamed = renames
            .tables
            .get(&n.name)
            .and_then(|from| old_by_name.get(from.as_str()))
            .filter(|o| !new_names.contains(o.name.as_str()) && !table_map.contains_key(o.name.as_str()));
        match renamed {
            Some(o) => {
                pairs.push((o, n));
                table_map.insert(&o.name, &n.name);
            }
            None => created.push(n),
        }
    }
    let dropped: Vec<&Table> = old.tables.iter().filter(|t| !table_map.contains_key(t.name.as_str())).collect();

    // Column matching (old -> new names) for every matched table.
    let mut col_map: HashMap<(&str, &str), &str> = HashMap::new();
    let mut col_pairs: Vec<Vec<(Option<&Column>, Option<&Column>)>> = vec![];
    for (o, n) in &pairs {
        let mut matched: Vec<(Option<&Column>, Option<&Column>)> = vec![];
        let mut used: HashSet<&str> = HashSet::new();
        for nc in &n.columns {
            let oc = o.column(&nc.name).or_else(|| {
                renames
                    .columns
                    .get(&(n.name.clone(), nc.name.clone()))
                    .and_then(|from| o.column(from))
                    .filter(|oc| n.column(&oc.name).is_none())
            });
            match oc {
                Some(oc) if !used.contains(oc.name.as_str()) => {
                    used.insert(&oc.name);
                    col_map.insert((&o.name, &oc.name), &nc.name);
                    matched.push((Some(oc), Some(nc)));
                }
                _ => matched.push((None, Some(nc))),
            }
        }
        for oc in &o.columns {
            if !used.contains(oc.name.as_str()) {
                matched.push((Some(oc), None));
            }
        }
        col_pairs.push(matched);
    }
    let renamed_old: Vec<Table> = pairs.iter().map(|(o, _)| apply_renames(o, &table_map, &col_map)).collect();

    let mut drop_triggers = vec![];
    let mut drop_fks = vec![];
    let mut drop_other = vec![];
    let mut renames_ops = vec![];
    let mut add_constraints = vec![];
    let mut add_fks = vec![];
    let mut create_indexes = vec![];
    let mut create_triggers = vec![];
    let mut comments = vec![];

    for ((o, n), ro) in pairs.iter().zip(&renamed_old) {
        let table = n.name.clone();

        // triggers: no rename by definition (names are user-chosen)
        for t in &o.triggers {
            let keep = n.triggers.iter().find(|x| x.name == t.name);
            let ro_t = ro.triggers.iter().find(|x| x.name == t.name);
            if keep.is_none() || keep != ro_t {
                drop_triggers.push(Op::DropTrigger { table: o.name.clone(), name: t.name.clone() });
            }
        }
        for t in &n.triggers {
            if ro.triggers.iter().find(|x| x.name == t.name) != Some(t) {
                create_triggers.push(Op::CreateTrigger { table: table.clone(), trigger: t.clone() });
            }
        }

        let (fk_new, fk_drop, fk_ren) =
            match_named(&ro.foreign_keys, &n.foreign_keys, |c| c.name.as_str(), |c, nm| {
                let mut c = c.clone();
                c.name = nm.into();
                c
            });
        drop_fks.extend(fk_drop.iter().map(|c| Op::DropConstraint { table: o.name.clone(), name: c.name.clone() }));
        renames_ops.extend(
            fk_ren
                .iter()
                .map(|(a, b)| Op::RenameConstraint { table: table.clone(), from: a.name.clone(), to: b.name.clone() }),
        );
        add_fks.extend(fk_new.iter().map(|c| Op::AddConstraint {
            table: table.clone(),
            constraint: Constraint::ForeignKey((*c).clone()),
        }));

        let (old_c, new_c) = (constraints(ro), constraints(n));
        let (c_new, c_drop, c_ren) = match_named(&old_c, &new_c, |c| c.name(), |c, nm| c.renamed(nm));
        drop_other.extend(c_drop.iter().map(|c| Op::DropConstraint { table: o.name.clone(), name: c.name().into() }));
        renames_ops.extend(
            c_ren
                .iter()
                .map(|(a, b)| Op::RenameConstraint { table: table.clone(), from: a.name().into(), to: b.name().into() }),
        );
        add_constraints
            .extend(c_new.iter().map(|c| Op::AddConstraint { table: table.clone(), constraint: (*c).clone() }));

        let (ix_new, ix_drop, ix_ren) = match_named(&ro.indexes, &n.indexes, |i| i.name.as_str(), |i, nm| {
            let mut i = i.clone();
            i.name = nm.into();
            i
        });
        drop_other.extend(ix_drop.iter().map(|i| Op::DropIndex { name: i.name.clone() }));
        renames_ops
            .extend(ix_ren.iter().map(|(a, b)| Op::RenameIndex { from: a.name.clone(), to: b.name.clone() }));
        create_indexes.extend(ix_new.iter().map(|i| Op::CreateIndex { table: table.clone(), index: (*i).clone() }));

        if o.comment != n.comment {
            comments.push(Op::CommentOnTable { table: table.clone(), comment: n.comment.clone() });
        }
    }
    // Foreign keys of dropped tables go first, so tables can then be dropped in any order.
    for t in &dropped {
        for fk in &t.foreign_keys {
            drop_fks.push(Op::DropConstraint { table: t.name.clone(), name: fk.name.clone() });
        }
    }

    ops.extend(drop_triggers);
    ops.extend(drop_fks);
    ops.extend(drop_other);

    // -- renames ---------------------------------------------------------------------------
    for (o, n) in &pairs {
        if o.name != n.name {
            ops.push(Op::RenameTable { from: o.name.clone(), to: n.name.clone() });
        }
    }
    for ((_, n), cols) in pairs.iter().zip(&col_pairs) {
        for (oc, nc) in cols {
            if let (Some(oc), Some(nc)) = (oc, nc) {
                if oc.name != nc.name {
                    ops.push(Op::RenameColumn { table: n.name.clone(), from: oc.name.clone(), to: nc.name.clone() });
                }
            }
        }
    }
    ops.extend(renames_ops);

    // -- tables ----------------------------------------------------------------------------
    for t in &dropped {
        ops.push(Op::DropTable { table: t.name.clone() });
    }
    let (order, deferred) = creation_order(&created);
    let deferred_names: BTreeSet<(String, String)> =
        deferred.iter().map(|(t, fk)| (t.clone(), fk.name.clone())).collect();
    for t in order {
        let (inline, _): (Vec<ForeignKey>, Vec<ForeignKey>) = t
            .foreign_keys
            .iter()
            .cloned()
            .partition(|fk| !deferred_names.contains(&(t.name.clone(), fk.name.clone())));
        let mut bare = t.clone();
        bare.foreign_keys.clear();
        bare.indexes.clear();
        bare.triggers.clear();
        ops.push(Op::CreateTable { table: bare, foreign_keys: inline });
        create_indexes.extend(t.indexes.iter().map(|i| Op::CreateIndex { table: t.name.clone(), index: i.clone() }));
        create_triggers
            .extend(t.triggers.iter().map(|tr| Op::CreateTrigger { table: t.name.clone(), trigger: tr.clone() }));
        if t.comment.is_some() {
            comments.push(Op::CommentOnTable { table: t.name.clone(), comment: t.comment.clone() });
        }
        for c in t.columns.iter().filter(|c| c.comment.is_some()) {
            comments.push(Op::CommentOnColumn { table: t.name.clone(), column: c.name.clone(), comment: c.comment.clone() });
        }
    }
    add_fks.extend(deferred.into_iter().map(|(table, fk)| Op::AddConstraint { table, constraint: Constraint::ForeignKey(fk) }));

    // -- columns ---------------------------------------------------------------------------
    let (mut adds, mut alters, mut drops) = (vec![], vec![], vec![]);
    for ((_, n), cols) in pairs.iter().zip(&col_pairs) {
        let table = &n.name;
        for (oc, nc) in cols {
            match (oc, nc) {
                (None, Some(nc)) => {
                    adds.push(Op::AddColumn { table: table.clone(), column: (*nc).clone() });
                    if nc.comment.is_some() {
                        comments.push(Op::CommentOnColumn {
                            table: table.clone(),
                            column: nc.name.clone(),
                            comment: nc.comment.clone(),
                        });
                    }
                }
                (Some(oc), None) => drops.push(Op::DropColumn { table: table.clone(), column: oc.name.clone() }),
                (Some(oc), Some(nc)) => {
                    let column = nc.name.clone();
                    let t = || table.clone();
                    if oc.identity && !nc.identity {
                        alters.push(Op::DropIdentity { table: t(), column: column.clone() });
                    }
                    if oc.default.is_some() && oc.default != nc.default {
                        alters.push(Op::DropDefault { table: t(), column: column.clone() });
                    }
                    if oc.ty != nc.ty {
                        alters.push(Op::AlterColumnType {
                            table: t(),
                            column: column.clone(),
                            from: oc.ty.clone(),
                            to: nc.ty.clone(),
                        });
                    }
                    if let Some(d) = nc.default.as_ref().filter(|_| oc.default != nc.default) {
                        alters.push(Op::SetDefault { table: t(), column: column.clone(), default: d.clone() });
                    }
                    if nc.identity && !oc.identity {
                        alters.push(Op::AddIdentity { table: t(), column: column.clone() });
                    }
                    match (oc.nullable, nc.nullable) {
                        (true, false) => alters.push(Op::SetNotNull { table: t(), column: column.clone() }),
                        (false, true) => alters.push(Op::DropNotNull { table: t(), column: column.clone() }),
                        _ => {}
                    }
                    if oc.comment != nc.comment {
                        comments.push(Op::CommentOnColumn { table: t(), column, comment: nc.comment.clone() });
                    }
                }
                (None, None) => {}
            }
        }
    }
    ops.extend(adds);
    ops.extend(alters);
    ops.extend(drops);

    // -- new dependent objects -------------------------------------------------------------
    // Primary keys and uniques first: foreign keys may reference them.
    add_constraints.sort_by_key(|op| match op {
        Op::AddConstraint { constraint: Constraint::PrimaryKey(_), .. } => 0,
        Op::AddConstraint { constraint: Constraint::Unique(_), .. } => 1,
        _ => 2,
    });
    ops.extend(add_constraints);
    ops.extend(add_fks);
    ops.extend(create_indexes);
    ops.extend(create_triggers);
    ops.extend(comments);

    for f in &old.functions {
        if !new_fns.contains_key(&f.key()) {
            ops.push(Op::DropFunction(f.clone()));
        }
    }
    for e in &old.extensions {
        if !new.extensions.iter().any(|n| n.name == e.name) {
            ops.push(Op::DropExtension(e.clone()));
        }
    }
    ops
}
