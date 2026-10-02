//! Language-independent ORM core shared by every binding (Python, Node, Go).
//!
//! Holds the entities, the queries and the Tokio runtime. Bindings only convert
//! their language's values to and from these types.

use std::sync::OnceLock;
use std::time::Instant;

use chrono::{DateTime, FixedOffset, Utc};
use sea_orm::{
    ActiveValue::{NotSet, Set},
    ColumnTrait, ConnectOptions, Database, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};

pub use sea_orm::DbErr;
pub use sea_orm::DatabaseConnection as Db;

pub mod author {
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

pub mod post {
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

pub type PostWithAuthor = (post::Model, Option<author::Model>);

/// A post to insert, already converted out of the caller's language.
#[derive(Clone)]
pub struct NewPost {
    pub author_id: i64,
    pub title: String,
    pub body: String,
    pub views: i32,
    pub published: bool,
    pub created_at: DateTime<FixedOffset>,
}

impl NewPost {
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

/// Shared multi-thread Tokio runtime for bindings that have none of their own
/// (the Go C ABI). Python and Node use their binding crate's runtime instead.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start Tokio runtime")
    })
}

pub async fn connect(url: &str, max_connections: u32) -> Result<Db, DbErr> {
    let mut opts = ConnectOptions::new(url);
    opts.max_connections(max_connections)
        .min_connections(1)
        .sqlx_logging(false);
    Database::connect(opts).await
}

pub async fn select_posts(db: &Db, limit: u64) -> Result<Vec<post::Model>, DbErr> {
    post::Entity::find()
        .order_by_asc(post::Column::Id)
        .limit(limit)
        .all(db)
        .await
}

pub async fn select_posts_with_author(db: &Db, limit: u64) -> Result<Vec<PostWithAuthor>, DbErr> {
    post::Entity::find()
        .find_also_related(author::Entity)
        .order_by_asc(post::Column::Id)
        .limit(limit)
        .all(db)
        .await
}

/// One `INSERT ... RETURNING id` for all rows.
pub async fn insert_many(db: &Db, rows: Vec<NewPost>) -> Result<Vec<i64>, DbErr> {
    post::Entity::insert_many(rows.into_iter().map(NewPost::into_active))
        .exec_with_returning_keys(db)
        .await
}

pub async fn insert_one(db: &Db, row: NewPost) -> Result<i64, DbErr> {
    post::Entity::insert(row.into_active())
        .exec(db)
        .await
        .map(|r| r.last_insert_id)
}

pub async fn delete_above(db: &Db, id: i64) -> Result<u64, DbErr> {
    post::Entity::delete_many()
        .filter(post::Column::Id.gt(id))
        .exec(db)
        .await
        .map(|r| r.rows_affected)
}

/// Pure-Rust baseline: runs `op` `warmup + iters` times inside Tokio, building no
/// foreign-language objects, and returns per-iteration wall time in ns.
///
/// ops: "read", "read_join", "write_bulk", "write_loop". Writes are cleaned up
/// (untimed) by deleting ids above `cleanup_above`.
pub async fn bench_loop(
    db: &Db,
    op: &str,
    n: usize,
    iters: usize,
    warmup: usize,
    cleanup_above: i64,
) -> Result<Vec<u64>, String> {
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
    let err = |e: DbErr| e.to_string();
    let mut out = Vec::with_capacity(iters);
    for i in 0..warmup + iters {
        let start = Instant::now();
        match op {
            "read" => {
                std::hint::black_box(select_posts(db, n as u64).await.map_err(err)?);
            }
            "read_join" => {
                std::hint::black_box(select_posts_with_author(db, n as u64).await.map_err(err)?);
            }
            "write_bulk" => {
                std::hint::black_box(insert_many(db, template.clone()).await.map_err(err)?);
            }
            "write_loop" => {
                for row in template.iter().cloned() {
                    std::hint::black_box(insert_one(db, row).await.map_err(err)?);
                }
            }
            _ => return Err(format!("unknown op {op}")),
        }
        let elapsed = start.elapsed().as_nanos() as u64;
        if op.starts_with("write") {
            delete_above(db, cleanup_above).await.map_err(err)?;
        }
        if i >= warmup {
            out.push(elapsed);
        }
    }
    Ok(out)
}
