//! Feasibility prototype: Python (asyncio) -> PyO3 -> SeaORM -> Postgres.
//!
//! Every public method is one FFI crossing that returns an awaitable. Results are
//! materialized into Python objects in a single batch pass once the query finishes.

use std::time::Instant;

use chrono::{DateTime, FixedOffset, Utc};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::{intern, IntoPyObjectExt};
use sea_orm::{
    ActiveValue::{NotSet, Set},
    ColumnTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbErr,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};

mod author {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "blog_author")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub email: String,
        pub created_at: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(has_many = "super::post::Entity")]
        Post,
    }

    impl Related<super::post::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Post.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

mod post {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "blog_post")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub author_id: i64,
        pub title: String,
        #[sea_orm(column_type = "Text")]
        pub body: String,
        pub views: i32,
        pub published: bool,
        pub created_at: DateTimeWithTimeZone,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::author::Entity",
            from = "Column::AuthorId",
            to = "super::author::Column::Id"
        )]
        Author,
    }

    impl Related<super::author::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Author.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

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

/// Plain Rust struct extracted from a Python dict before the query runs.
#[derive(Clone)]
struct NewPost {
    author_id: i64,
    title: String,
    body: String,
    views: i32,
    published: bool,
    created_at: DateTime<FixedOffset>,
}

impl NewPost {
    fn extract(d: &Bound<'_, PyAny>) -> PyResult<Self> {
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

    fn into_active(self) -> post::ActiveModel {
        post::ActiveModel {
            id: NotSet,
            author_id: Set(self.author_id),
            title: Set(self.title),
            body: Set(self.body),
            views: Set(self.views),
            published: Set(self.published),
            created_at: Set(self.created_at),
        }
    }
}

async fn select_posts(db: &DatabaseConnection, limit: u64) -> Result<Vec<post::Model>, DbErr> {
    post::Entity::find()
        .order_by_asc(post::Column::Id)
        .limit(limit)
        .all(db)
        .await
}

async fn select_posts_with_author(
    db: &DatabaseConnection,
    limit: u64,
) -> Result<Vec<(post::Model, Option<author::Model>)>, DbErr> {
    post::Entity::find()
        .find_also_related(author::Entity)
        .order_by_asc(post::Column::Id)
        .limit(limit)
        .all(db)
        .await
}

async fn insert_many(db: &DatabaseConnection, rows: Vec<NewPost>) -> Result<Vec<i64>, DbErr> {
    post::Entity::insert_many(rows.into_iter().map(NewPost::into_active))
        .exec_with_returning_keys(db)
        .await
}

async fn delete_above(db: &DatabaseConnection, id: i64) -> Result<u64, DbErr> {
    post::Entity::delete_many()
        .filter(post::Column::Id.gt(id))
        .exec(db)
        .await
        .map(|r| r.rows_affected)
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
            let rows = select_posts(&db, limit).await.map_err(db_err)?;
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
            let rows = select_posts_with_author(&db, limit).await.map_err(db_err)?;
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
            .map(|r| NewPost::extract(&r))
            .collect::<PyResult<Vec<_>>>()?;
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            insert_many(&db, rows).await.map_err(db_err)
        })
    }

    /// `await client.insert_post(dict)` -> new id. Used to measure per-row FFI round trips.
    fn insert_post<'py>(&self, py: Python<'py>, row: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let row = NewPost::extract(row)?;
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            post::Entity::insert(row.into_active())
                .exec(&db)
                .await
                .map(|r| r.last_insert_id)
                .map_err(db_err)
        })
    }

    /// `await client.delete_posts_above(id)` -> rows deleted. Benchmark cleanup helper.
    fn delete_posts_above<'py>(&self, py: Python<'py>, id: i64) -> PyResult<Bound<'py, PyAny>> {
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            delete_above(&db, id).await.map_err(db_err)
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
            let template: Vec<NewPost> = (0..n)
                .map(|i| NewPost {
                    author_id: (i % 50) as i64 + 1,
                    title: format!("Bench post {i}"),
                    body: "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(4),
                    views: i as i32,
                    published: i % 2 == 0,
                    created_at: Utc::now().fixed_offset(),
                })
                .collect();
            let mut out = Vec::with_capacity(iters);
            for i in 0..warmup + iters {
                let start = Instant::now();
                match op.as_str() {
                    "read" => {
                        let rows = select_posts(&db, n as u64).await.map_err(db_err)?;
                        std::hint::black_box(rows);
                    }
                    "read_join" => {
                        let rows = select_posts_with_author(&db, n as u64).await.map_err(db_err)?;
                        std::hint::black_box(rows);
                    }
                    "write_bulk" => {
                        let ids = insert_many(&db, template.clone()).await.map_err(db_err)?;
                        std::hint::black_box(ids);
                    }
                    "write_loop" => {
                        for row in template.iter().cloned() {
                            let r = post::Entity::insert(row.into_active())
                                .exec(&db)
                                .await
                                .map_err(db_err)?;
                            std::hint::black_box(r);
                        }
                    }
                    _ => return Err(PyValueError::new_err(format!("unknown op {op}"))),
                }
                let elapsed = start.elapsed().as_nanos() as u64;
                if op.starts_with("write") {
                    delete_above(&db, cleanup_above).await.map_err(db_err)?;
                }
                if i >= warmup {
                    out.push(elapsed);
                }
            }
            Ok(out)
        })
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
        let mut opts = ConnectOptions::new(url);
        opts.max_connections(max_connections)
            .min_connections(1)
            .sqlx_logging(false);
        let db = Database::connect(opts).await.map_err(db_err)?;
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
