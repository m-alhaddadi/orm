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

use crate::convert::cell_to_py;
use crate::errors::db_err;
use orm_core::ir::ValueType;
use orm_core::schema::Schema;
use orm_engine::db::{Cell, RowSet};
use orm_engine::exec::Fetched;
use orm_engine::plan::{JoinShape, Output};

struct ModelClass {
    cls: Py<PyType>,
    /// Field names, interned, in schema order (the order of the row columns).
    names: Vec<Py<PyString>>,
}

/// The Python class of each model, by schema model index, and the members of each
/// enum by stored value.
pub struct Classes {
    #[cfg(feature = "proxy-models")]
    pub proxies: Vec<orm_core::proxy::PreparedProxy>,
    #[cfg(feature = "composition")]
    pub native: Vec<orm_core::behavior::NativeModel>,
    models: Vec<Option<ModelClass>>,
    enums: Vec<Option<Py<PyDict>>>,
}

impl Classes {
    pub fn empty() -> Self {
        Classes { models: vec![], enums: vec![],
            #[cfg(feature = "proxy-models")] proxies: vec![],
            #[cfg(feature = "composition")] native: vec![] }
    }

    /// `classes` maps model names to their classes, and enum names to their enum
    /// classes (iterating one gives its members, `member.value` is what is stored).
    pub fn new(py: Python<'_>, schema: &Schema, classes: &Bound<'_, PyDict>) -> PyResult<Self> {
        let mut enums = Vec::with_capacity(schema.enums.len());
        for e in &schema.enums {
            enums.push(match classes.get_item(&e.name)? {
                None => None,
                Some(cls) => {
                    let members = PyDict::new(py);
                    for m in cls.try_iter()? {
                        let m = m?;
                        members.set_item(m.getattr(pyo3::intern!(py, "value"))?, m)?;
                    }
                    Some(members.unbind())
                }
            });
        }
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
        Ok(Classes { models: out, enums,
            #[cfg(feature = "proxy-models")] proxies: schema.proxy_models.clone(),
            #[cfg(feature = "composition")] native: schema.models.iter().map(|m| m.native).collect() })
    }

    fn get(&self, model: usize) -> PyResult<&ModelClass> {
        self.models
            .get(model)
            .and_then(Option::as_ref)
            .ok_or_else(|| PyTypeError::new_err("the schema was compiled without model classes"))
    }
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

    /// One cell, enum values as their members (values the enum doesn't know stay as
    /// they are).
    fn cell(&self, rows: &dyn RowSet, r: usize, c: usize, ty: ValueType) -> PyResult<Py<PyAny>> {
        let v = raw_cell(self.py, rows, r, c, ty)?;
        let Some(members) = ty.enum_idx.and_then(|i| self.classes.enums.get(i as usize)).and_then(Option::as_ref) else {
            return Ok(v);
        };
        let members = members.bind(self.py);
        let member = |v: Bound<'py, PyAny>| -> PyResult<Bound<'py, PyAny>> {
            Ok(members.get_item(&v)?.unwrap_or(v))
        };
        let v = v.into_bound(self.py);
        if ty.array && !v.is_none() {
            let out = PyList::empty(self.py);
            for item in v.try_iter()? {
                out.append(member(item?)?)?;
            }
            return Ok(out.into_any().unbind());
        }
        Ok(member(v)?.unbind())
    }

    /// An instance of `model` from row `r`, its fields in columns `start..`.
    fn instance(&self, model: usize, rows: &dyn RowSet, r: usize, start: usize, types: &[ValueType]) -> PyResult<Instance<'py>> {
        let mc = self.classes.get(model)?;
        let inst = self.blank(mc)?;
        for (i, name) in mc.names.iter().enumerate() {
            inst.dict.set_item(name.bind(self.py), self.cell(rows, r, start + i, types[start + i])?)?;
        }
        Ok(inst)
    }

    /// One instance per row, `select_related` objects attached.
    fn instances(&self, model: usize, joins: &[JoinShape], rows: &dyn RowSet, types: &[ValueType]) -> PyResult<Vec<Instance<'py>>> {
        let mut out = Vec::with_capacity(rows.len());
        let mut related: Vec<Option<Instance<'py>>> = Vec::with_capacity(joins.len());
        for r in 0..rows.len() {
            let root = self.instance(model, rows, r, 0, types)?;
            related.clear();
            for j in joins {
                // A LEFT JOIN without a match yields NULLs, including the primary key.
                let pk = rows.cell(r, j.start + j.pk_pos, types[j.start + j.pk_pos]).map_err(db_err)?;
                let child = if pk == Cell::Null { None } else { Some(self.instance(j.model, rows, r, j.start, types)?) };
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
        types: &[ValueType],
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
                                values.push(self.cell(rows, r, pos, types[pos])?);
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
    pub fn model_rows(&self, model: usize, rows: &dyn RowSet, types: &[ValueType]) -> PyResult<Bound<'py, PyList>> {
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
            let key = raw_cell(py, f.rows.as_ref(), i, p.child_key_pos, key_type)?;
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
            let key = raw_cell(py, parent_rows, i, p.key_pos, p.key_type)?;
            let found = if key.is_none(py) { None } else { groups.get_item(&key)? };
            if !p.many {
                if let (Some(back), Some(child)) = (&p.back, &found) {
                    // SAFETY: as in `blank`.
                    let d = unsafe {
                        Bound::from_owned_ptr_or_err(py, ffi::PyObject_GenericGetDict(child.as_ptr(), std::ptr::null_mut()))?
                            .cast_into_unchecked::<PyDict>()
                    };
                    d.set_item(back, &parent.obj)?;
                }
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

/// One cell as a Python value, enum values as stored.
fn raw_cell(py: Python<'_>, rows: &dyn RowSet, r: usize, c: usize, ty: ValueType) -> PyResult<Py<PyAny>> {
    cell_to_py(py, rows.cell(r, c, ty).map_err(db_err)?)
}
