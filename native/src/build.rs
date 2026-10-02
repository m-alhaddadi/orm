//! Python objects from result rows, built in one pass while holding the GIL: model
//! instances (with `select_related` objects and prefetched relations attached) and
//! `select()` rows.
//!
//! An instance is made the way `cls.__new__(cls)` makes it, then its fields go straight
//! into its `__dict__`, so no Python code runs per row. Relations go into `__dict__`
//! too, which bypasses the read-only relation descriptors.

use pyo3::exceptions::PyTypeError;
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyTuple, PyType};

use crate::db::RowSet;
use crate::plan::{JoinShape, Output, PrefetchPlan};
use orm_core::ir::ColType;
use orm_core::schema::Schema;

struct ModelClass {
    cls: Py<PyType>,
    /// Field names, interned, in schema order (the order of the row columns).
    names: Vec<Py<PyString>>,
}

/// The Python class of each model, by schema model index.
pub struct Classes(Vec<Option<ModelClass>>);

impl Classes {
    pub fn empty() -> Self {
        Classes(vec![])
    }

    /// `classes` maps model names to their classes.
    pub fn new(py: Python<'_>, schema: &Schema, classes: &Bound<'_, PyDict>) -> PyResult<Self> {
        let mut out = Vec::with_capacity(schema.models.len());
        for m in &schema.models {
            out.push(match classes.get_item(&m.ir.name)? {
                None => None,
                Some(cls) => Some(ModelClass {
                    cls: cls.cast_into::<PyType>()?.unbind(),
                    names: m.fields().iter().map(|f| PyString::intern(py, &f.name).unbind()).collect(),
                }),
            });
        }
        Ok(Classes(out))
    }

    fn get(&self, model: usize) -> PyResult<&ModelClass> {
        self.0
            .get(model)
            .and_then(Option::as_ref)
            .ok_or_else(|| PyTypeError::new_err("the schema was compiled without model classes"))
    }
}

/// A related set fetched for parent rows, and the sets fetched for its own rows.
pub struct Fetched {
    pub plan: PrefetchPlan,
    pub rows: Box<dyn RowSet>,
    pub children: Vec<Fetched>,
}

pub struct Builder<'a, 'py> {
    py: Python<'py>,
    classes: &'a Classes,
    /// The database instances write back to (`using(db)`); stored as `_db`.
    db: Option<&'a Bound<'py, PyAny>>,
    no_args: Bound<'py, PyTuple>,
}

struct Instance<'py> {
    obj: Bound<'py, PyAny>,
    dict: Bound<'py, PyDict>,
}

impl<'a, 'py> Builder<'a, 'py> {
    pub fn new(py: Python<'py>, classes: &'a Classes, db: Option<&'a Bound<'py, PyAny>>) -> Self {
        Builder { py, classes, db, no_args: PyTuple::empty(py) }
    }

    /// An empty instance of `mc` and its `__dict__`.
    fn blank(&self, mc: &ModelClass) -> PyResult<Instance<'py>> {
        let py = self.py;
        // SAFETY: `PyType_GenericNew` is what `object.__new__(cls)` does for a class
        // without its own `__new__` (models never define one); `PyObject_GenericGetDict`
        // returns (creating it if needed) the instance dict. Both return new references.
        unsafe {
            let tp = mc.cls.bind(py).as_type_ptr();
            let obj = Bound::from_owned_ptr_or_err(py, ffi::PyType_GenericNew(tp, self.no_args.as_ptr(), std::ptr::null_mut()))?;
            let dict = Bound::from_owned_ptr_or_err(py, ffi::PyObject_GenericGetDict(obj.as_ptr(), std::ptr::null_mut()))?
                .cast_into_unchecked::<PyDict>();
            if let Some(db) = self.db {
                dict.set_item(pyo3::intern!(py, "_db"), db)?;
            }
            Ok(Instance { obj, dict })
        }
    }

    /// An instance of `model` from row `r`, its fields in columns `start..`.
    fn instance(&self, model: usize, rows: &dyn RowSet, r: usize, start: usize, types: &[ColType]) -> PyResult<Instance<'py>> {
        let mc = self.classes.get(model)?;
        let inst = self.blank(mc)?;
        for (i, name) in mc.names.iter().enumerate() {
            inst.dict.set_item(name.bind(self.py), rows.cell(self.py, r, start + i, types[start + i])?)?;
        }
        Ok(inst)
    }

    /// One instance per row, `select_related` objects attached.
    fn instances(&self, model: usize, joins: &[JoinShape], rows: &dyn RowSet, types: &[ColType]) -> PyResult<Vec<Instance<'py>>> {
        let mut out = Vec::with_capacity(rows.len());
        let mut related: Vec<Option<Instance<'py>>> = Vec::with_capacity(joins.len());
        for r in 0..rows.len() {
            let root = self.instance(model, rows, r, 0, types)?;
            related.clear();
            for j in joins {
                // A LEFT JOIN without a match yields NULLs, including the primary key.
                let pk = rows.cell(self.py, r, j.start + j.pk_pos, types[j.start + j.pk_pos])?;
                let child = if pk.is_none(self.py) { None } else { Some(self.instance(j.model, rows, r, j.start, types)?) };
                let parent = match j.parent {
                    None => Some(&root),
                    Some(p) => related[p].as_ref(),
                };
                if let Some(parent) = parent {
                    parent.dict.set_item(&j.attr, child.as_ref().map(|c| &c.obj))?;
                }
                related.push(child);
            }
            out.push(root);
        }
        Ok(out)
    }

    /// The objects of a top-level SELECT: instances (prefetched relations attached) or
    /// `row_cls` rows.
    pub fn select(
        &self,
        output: &Output,
        rows: &dyn RowSet,
        types: &[ColType],
        prefetched: &[Fetched],
        row_cls: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyList>> {
        match output {
            Output::Instances { model, joins } => {
                let objs = self.instances(*model, joins, rows, types)?;
                for f in prefetched {
                    self.attach(&objs, rows, f)?;
                }
                PyList::new(self.py, objs.into_iter().map(|i| i.obj))
            }
            Output::Rows { model, items } => {
                let out = PyList::empty(self.py);
                let mut values = Vec::with_capacity(items.len());
                for r in 0..rows.len() {
                    values.clear();
                    let mut pos = 0;
                    for item in items {
                        match item {
                            Some(width) => {
                                values.push(self.instance(*model, rows, r, pos, types)?.obj.unbind());
                                pos += width;
                            }
                            None => {
                                values.push(rows.cell(self.py, r, pos, types[pos])?);
                                pos += 1;
                            }
                        }
                    }
                    let t = PyTuple::new(self.py, values.drain(..))?;
                    match row_cls {
                        Some(cls) => out.append(cls.call1((t,))?)?,
                        None => out.append(t)?,
                    }
                }
                Ok(out)
            }
        }
    }

    /// Instances of `model` for rows returned by a write (`RETURNING`).
    pub fn model_rows(&self, model: usize, rows: &dyn RowSet, types: &[ColType]) -> PyResult<Bound<'py, PyList>> {
        PyList::new(self.py, self.instances(model, &[], rows, types)?.into_iter().map(|i| i.obj))
    }

    /// Puts the related objects of `f` on `parents` (built from `parent_rows`, in order).
    fn attach(&self, parents: &[Instance<'py>], parent_rows: &dyn RowSet, f: &Fetched) -> PyResult<()> {
        let py = self.py;
        let p = &f.plan;
        let Output::Instances { model, joins } = &p.output else {
            return Err(PyTypeError::new_err("prefetch rows must be instances"));
        };
        let children = self.instances(*model, joins, f.rows.as_ref(), &p.types)?;
        for c in &f.children {
            self.attach(&children, f.rows.as_ref(), c)?;
        }
        let key_type = p.types[p.child_key_pos];
        let groups = PyDict::new(py);
        for (i, c) in children.iter().enumerate() {
            let key = f.rows.cell(py, i, p.child_key_pos, key_type)?;
            if p.many {
                match groups.get_item(&key)? {
                    Some(list) => list.cast_into::<PyList>()?.append(&c.obj)?,
                    None => groups.set_item(key, PyList::new(py, [&c.obj])?)?,
                }
            } else {
                groups.set_item(key, &c.obj)?;
            }
        }
        let attr = PyString::intern(py, &p.attr);
        for (i, parent) in parents.iter().enumerate() {
            let key = parent_rows.cell(py, i, p.key_pos, p.key_type)?;
            let found = if key.is_none(py) { None } else { groups.get_item(&key)? };
            if !p.many {
                parent.dict.set_item(&attr, found)?;
                continue;
            }
            let list = match found {
                Some(list) => list.cast_into::<PyList>()?,
                None => PyList::empty(py),
            };
            if let Some(back) = &p.back {
                for c in list.iter() {
                    // SAFETY: as in `blank`.
                    let d = unsafe {
                        Bound::from_owned_ptr_or_err(py, ffi::PyObject_GenericGetDict(c.as_ptr(), std::ptr::null_mut()))?
                            .cast_into_unchecked::<PyDict>()
                    };
                    d.set_item(back, &parent.obj)?;
                }
            }
            parent.dict.set_item(&attr, list)?;
        }
        Ok(())
    }
}
