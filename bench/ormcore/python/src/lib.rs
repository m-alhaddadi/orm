//! Python binding: Python (asyncio or sync) -> PyO3 -> ormcore-core (SeaORM) -> Postgres.
//!
//! Every public method is one FFI crossing that returns an awaitable. Results are
//! materialized into Python objects in a single batch pass once the query finishes.

use chrono::{DateTime, FixedOffset, Utc};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::{intern, IntoPyObjectExt};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr};

use ormcore_core::{author, post, NewPost};

fn db_err(e: DbErr) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

/// Python-side model object. Fields are converted to Python objects once, at
/// materialization time, so attribute access afterwards is a plain reference.
#[pyclass(frozen, name = "Author", module = "ormcore")]
struct AuthorObj {
    #[pyo3(get)]
    id: i64,
    #[pyo3(get)]
    name: Py<PyAny>,
    #[pyo3(get)]
    email: Py<PyAny>,
    #[pyo3(get)]
    created_at: Py<PyAny>,
}

#[pyclass(frozen, name = "Post", module = "ormcore")]
struct PostObj {
    #[pyo3(get)]
    id: i64,
    #[pyo3(get)]
    author_id: i64,
    #[pyo3(get)]
    title: Py<PyAny>,
    #[pyo3(get)]
    body: Py<PyAny>,
    #[pyo3(get)]
    views: i32,
    #[pyo3(get)]
    published: bool,
    #[pyo3(get)]
    created_at: Py<PyAny>,
    #[pyo3(get)]
    author: Option<Py<AuthorObj>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Dict,
    Obj,
}

impl Mode {
    fn parse(s: &str) -> PyResult<Self> {
        match s {
            "dict" => Ok(Mode::Dict),
            "obj" => Ok(Mode::Obj),
            _ => Err(PyValueError::new_err("mode must be 'dict' or 'obj'")),
        }
    }
}

/// Postgres hands back `timestamptz` in UTC; converting through `DateTime<Utc>` lets
/// PyO3 reuse the `datetime.timezone.utc` singleton instead of building a tzinfo per row.
fn py_dt(py: Python<'_>, dt: &DateTime<FixedOffset>) -> PyResult<Py<PyAny>> {
    dt.with_timezone(&Utc).into_py_any(py)
}

fn author_to_py(py: Python<'_>, a: &author::Model, mode: Mode) -> PyResult<Py<PyAny>> {
    match mode {
        Mode::Dict => {
            let d = PyDict::new(py);
            d.set_item(intern!(py, "id"), a.id)?;
            d.set_item(intern!(py, "name"), &a.name)?;
            d.set_item(intern!(py, "email"), &a.email)?;
            d.set_item(intern!(py, "created_at"), py_dt(py, &a.created_at)?)?;
            Ok(d.into_any().unbind())
        }
        Mode::Obj => Py::new(
            py,
            AuthorObj {
                id: a.id,
                name: a.name.as_str().into_py_any(py)?,
                email: a.email.as_str().into_py_any(py)?,
                created_at: py_dt(py, &a.created_at)?,
            },
        )
        .map(|o| o.into_any()),
    }
}

fn post_to_py(
    py: Python<'_>,
    p: &post::Model,
    author: Option<&author::Model>,
    mode: Mode,
) -> PyResult<Py<PyAny>> {
    match mode {
        Mode::Dict => {
            let d = PyDict::new(py);
            d.set_item(intern!(py, "id"), p.id)?;
            d.set_item(intern!(py, "author_id"), p.author_id)?;
            d.set_item(intern!(py, "title"), &p.title)?;
            d.set_item(intern!(py, "body"), &p.body)?;
            d.set_item(intern!(py, "views"), p.views)?;
            d.set_item(intern!(py, "published"), p.published)?;
            d.set_item(intern!(py, "created_at"), py_dt(py, &p.created_at)?)?;
            if let Some(a) = author {
                d.set_item(intern!(py, "author"), author_to_py(py, a, mode)?)?;
            }
            Ok(d.into_any().unbind())
        }
        Mode::Obj => {
            let author = match author {
                Some(a) => Some(
                    author_to_py(py, a, mode)?
                        .into_bound(py)
                        .cast_into::<AuthorObj>()?
                        .unbind(),
                ),
                None => None,
            };
            Py::new(
                py,
                PostObj {
                    id: p.id,
                    author_id: p.author_id,
                    title: p.title.as_str().into_py_any(py)?,
                    body: p.body.as_str().into_py_any(py)?,
                    views: p.views,
                    published: p.published,
                    created_at: py_dt(py, &p.created_at)?,
                    author,
                },
            )
            .map(|o| o.into_any())
        }
    }
}

/// Convert a Python dict into the core's `NewPost` before the query runs.
fn extract_new_post(d: &Bound<'_, PyAny>) -> PyResult<NewPost> {
    let py = d.py();
    Ok(NewPost {
        author_id: d.get_item(intern!(py, "author_id"))?.extract()?,
        title: d.get_item(intern!(py, "title"))?.extract()?,
        body: d.get_item(intern!(py, "body"))?.extract()?,
        views: d.get_item(intern!(py, "views"))?.extract()?,
        published: d.get_item(intern!(py, "published"))?.extract()?,
        created_at: d.get_item(intern!(py, "created_at"))?.extract()?,
    })
}

/// Run a future to completion on the shared Tokio runtime, GIL released.
fn block_on<F, T>(py: Python<'_>, fut: F) -> PyResult<T>
where
    F: std::future::Future<Output = Result<T, DbErr>> + Send,
    T: Send,
{
    py.detach(|| pyo3_async_runtimes::tokio::get_runtime().block_on(fut))
        .map_err(db_err)
}

#[pyclass(frozen, module = "ormcore")]
struct Client {
    db: DatabaseConnection,
}

#[pymethods]
impl Client {
    /// `await client.fetch_posts(limit, mode)` -> list of dicts or Post objects.
    #[pyo3(signature = (limit, mode = "obj"))]
    fn fetch_posts<'py>(&self, py: Python<'py>, limit: u64, mode: &str) -> PyResult<Bound<'py, PyAny>> {
        let mode = Mode::parse(mode)?;
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let rows = ormcore_core::select_posts(&db, limit).await.map_err(db_err)?;
            Python::attach(|py| {
                let items = rows
                    .iter()
                    .map(|p| post_to_py(py, p, None, mode))
                    .collect::<PyResult<Vec<_>>>()?;
                Ok(PyList::new(py, items)?.unbind())
            })
        })
    }

    /// `await client.fetch_posts_with_author(limit, mode)` -> posts with `author` joined.
    #[pyo3(signature = (limit, mode = "obj"))]
    fn fetch_posts_with_author<'py>(
        &self,
        py: Python<'py>,
        limit: u64,
        mode: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mode = Mode::parse(mode)?;
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let rows = ormcore_core::select_posts_with_author(&db, limit).await.map_err(db_err)?;
            Python::attach(|py| {
                let items = rows
                    .iter()
                    .map(|(p, a)| post_to_py(py, p, a.as_ref(), mode))
                    .collect::<PyResult<Vec<_>>>()?;
                Ok(PyList::new(py, items)?.unbind())
            })
        })
    }

    /// `await client.insert_posts([dict, ...])` -> list of new ids. One INSERT ... RETURNING.
    fn insert_posts<'py>(&self, py: Python<'py>, rows: &Bound<'py, PyList>) -> PyResult<Bound<'py, PyAny>> {
        let rows = rows
            .iter()
            .map(|r| extract_new_post(&r))
            .collect::<PyResult<Vec<_>>>()?;
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            ormcore_core::insert_many(&db, rows).await.map_err(db_err)
        })
    }

    /// `await client.insert_post(dict)` -> new id. Used to measure per-row FFI round trips.
    fn insert_post<'py>(&self, py: Python<'py>, row: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let row = extract_new_post(row)?;
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            ormcore_core::insert_one(&db, row).await
                .map_err(db_err)
        })
    }

    /// `await client.delete_posts_above(id)` -> rows deleted. Benchmark cleanup helper.
    fn delete_posts_above<'py>(&self, py: Python<'py>, id: i64) -> PyResult<Bound<'py, PyAny>> {
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            ormcore_core::delete_above(&db, id).await.map_err(db_err)
        })
    }

    /// Pure-Rust baseline: runs `op` `iters` times entirely inside Tokio (no Python
    /// objects built, no event-loop hops) and returns per-iteration wall time in ns.
    ///
    /// ops: "read", "read_join", "write_bulk", "write_loop".
    #[pyo3(signature = (op, n, iters, warmup, cleanup_above))]
    fn rust_bench<'py>(
        &self,
        py: Python<'py>,
        op: String,
        n: usize,
        iters: usize,
        warmup: usize,
        cleanup_above: i64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            ormcore_core::bench_loop(&db, &op, n, iters, warmup, cleanup_above)
                .await
                .map_err(PyRuntimeError::new_err)
        })
    }

    // --- sync API -------------------------------------------------------------
    // Same queries, but the calling Python thread blocks on the Tokio runtime with the
    // GIL released, so there is no asyncio <-> Tokio hand-off.

    /// `client.fetch_posts_sync(limit, mode)` -> list of dicts or Post objects.
    #[pyo3(signature = (limit, mode = "obj"))]
    fn fetch_posts_sync(&self, py: Python<'_>, limit: u64, mode: &str) -> PyResult<Py<PyList>> {
        let mode = Mode::parse(mode)?;
        let rows = block_on(py, ormcore_core::select_posts(&self.db, limit))?;
        let items = rows
            .iter()
            .map(|p| post_to_py(py, p, None, mode))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(PyList::new(py, items)?.unbind())
    }

    /// `client.fetch_posts_with_author_sync(limit, mode)` -> posts with `author` joined.
    #[pyo3(signature = (limit, mode = "obj"))]
    fn fetch_posts_with_author_sync(&self, py: Python<'_>, limit: u64, mode: &str) -> PyResult<Py<PyList>> {
        let mode = Mode::parse(mode)?;
        let rows = block_on(py, ormcore_core::select_posts_with_author(&self.db, limit))?;
        let items = rows
            .iter()
            .map(|(p, a)| post_to_py(py, p, a.as_ref(), mode))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(PyList::new(py, items)?.unbind())
    }

    /// `client.insert_posts_sync([dict, ...])` -> list of new ids.
    fn insert_posts_sync(&self, py: Python<'_>, rows: &Bound<'_, PyList>) -> PyResult<Vec<i64>> {
        let rows = rows
            .iter()
            .map(|r| extract_new_post(&r))
            .collect::<PyResult<Vec<_>>>()?;
        block_on(py, ormcore_core::insert_many(&self.db, rows))
    }

    /// `client.insert_post_sync(dict)` -> new id.
    fn insert_post_sync(&self, py: Python<'_>, row: &Bound<'_, PyAny>) -> PyResult<i64> {
        let row = extract_new_post(row)?;
        block_on(py, async {
            ormcore_core::insert_one(&self.db, row).await
        })
    }

    /// `client.delete_posts_above_sync(id)` -> rows deleted.
    fn delete_posts_above_sync(&self, py: Python<'_>, id: i64) -> PyResult<u64> {
        block_on(py, ormcore_core::delete_above(&self.db, id))
    }

    /// `client.noop_sync()` -> None. Measures the bare block_on round trip.
    fn noop_sync(&self, py: Python<'_>) -> PyResult<()> {
        block_on(py, async { Ok(()) })
    }

    /// `await client.noop()` -> None. Measures the bare asyncio <-> Tokio bridge cost.
    fn noop<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async { Ok(()) })
    }

    /// `await client.execute(sql)` -> rows affected. Raw escape hatch for setup.
    fn execute<'py>(&self, py: Python<'py>, sql: String) -> PyResult<Bound<'py, PyAny>> {
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            db.execute_unprepared(&sql)
                .await
                .map(|r| r.rows_affected())
                .map_err(db_err)
        })
    }
}

/// `client = await ormcore.connect(url, max_connections=1)`
#[pyfunction]
#[pyo3(signature = (url, max_connections = 1))]
fn connect(py: Python<'_>, url: String, max_connections: u32) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let db = ormcore_core::connect(&url, max_connections).await.map_err(db_err)?;
        Ok(Client { db })
    })
}

#[pymodule]
fn ormcore(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(connect, m)?)?;
    m.add_class::<Client>()?;
    m.add_class::<PostObj>()?;
    m.add_class::<AuthorObj>()?;
    Ok(())
}
