//! Node / Bun binding: JS -> napi-rs -> ormcore-core (SeaORM) -> Postgres.
//!
//! Async methods return Promises driven by napi's Tokio runtime; `*Sync` methods block
//! the JS thread on that runtime. Results are converted to plain JS objects in one batch
//! when the Promise resolves.

use chrono::{DateTime, Utc};
use napi::bindgen_prelude::block_on;
use napi::{Error, Result};
use napi_derive::napi;
use ormcore_core::{author, post, Db, DbErr, NewPost};

fn err(e: DbErr) -> Error {
    Error::from_reason(e.to_string())
}

#[napi(object)]
pub struct Author {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub created_at: DateTime<Utc>,
}

#[napi(object)]
pub struct Post {
    pub id: i64,
    pub author_id: i64,
    pub title: String,
    pub body: String,
    pub views: i32,
    pub published: bool,
    pub created_at: DateTime<Utc>,
    pub author: Option<Author>,
}

#[napi(object)]
pub struct NewPostInput {
    pub author_id: i64,
    pub title: String,
    pub body: String,
    pub views: i32,
    pub published: bool,
    pub created_at: DateTime<Utc>,
}

impl From<NewPostInput> for NewPost {
    fn from(p: NewPostInput) -> Self {
        NewPost {
            author_id: p.author_id,
            title: p.title,
            body: p.body,
            views: p.views,
            published: p.published,
            created_at: p.created_at.fixed_offset(),
        }
    }
}

fn to_author(a: author::Model) -> Author {
    Author {
        id: a.id,
        name: a.name,
        email: a.email,
        created_at: a.created_at.with_timezone(&Utc),
    }
}

fn to_post(p: post::Model, a: Option<author::Model>) -> Post {
    Post {
        id: p.id,
        author_id: p.author_id,
        title: p.title,
        body: p.body,
        views: p.views,
        published: p.published,
        created_at: p.created_at.with_timezone(&Utc),
        author: a.map(to_author),
    }
}

fn posts(rows: Vec<post::Model>) -> Vec<Post> {
    rows.into_iter().map(|p| to_post(p, None)).collect()
}

fn posts_with_author(rows: Vec<ormcore_core::PostWithAuthor>) -> Vec<Post> {
    rows.into_iter().map(|(p, a)| to_post(p, a)).collect()
}

#[napi]
pub struct Client {
    db: Db,
}

#[napi]
impl Client {
    #[napi]
    pub async fn fetch_posts(&self, limit: u32) -> Result<Vec<Post>> {
        ormcore_core::select_posts(&self.db, limit as u64)
            .await
            .map(posts)
            .map_err(err)
    }

    #[napi]
    pub async fn fetch_posts_with_author(&self, limit: u32) -> Result<Vec<Post>> {
        ormcore_core::select_posts_with_author(&self.db, limit as u64)
            .await
            .map(posts_with_author)
            .map_err(err)
    }

    /// One `INSERT ... RETURNING id`. JS objects are converted to Rust before the
    /// Promise starts.
    #[napi]
    pub async fn insert_posts(&self, rows: Vec<NewPostInput>) -> Result<Vec<i64>> {
        let rows = rows.into_iter().map(NewPost::from).collect();
        ormcore_core::insert_many(&self.db, rows).await.map_err(err)
    }

    #[napi]
    pub async fn insert_post(&self, row: NewPostInput) -> Result<i64> {
        ormcore_core::insert_one(&self.db, row.into()).await.map_err(err)
    }

    #[napi]
    pub async fn delete_posts_above(&self, id: i64) -> Result<i64> {
        ormcore_core::delete_above(&self.db, id)
            .await
            .map(|n| n as i64)
            .map_err(err)
    }

    /// Resolves immediately: measures the bare Promise <-> Tokio bridge cost.
    #[napi]
    pub async fn noop(&self) -> Result<()> {
        Ok(())
    }

    // --- sync API: blocks the JS thread on the Tokio runtime ---------------------

    #[napi]
    pub fn fetch_posts_sync(&self, limit: u32) -> Result<Vec<Post>> {
        block_on(ormcore_core::select_posts(&self.db, limit as u64))
            .map(posts)
            .map_err(err)
    }

    #[napi]
    pub fn fetch_posts_with_author_sync(&self, limit: u32) -> Result<Vec<Post>> {
        block_on(ormcore_core::select_posts_with_author(&self.db, limit as u64))
            .map(posts_with_author)
            .map_err(err)
    }

    #[napi]
    pub fn insert_posts_sync(&self, rows: Vec<NewPostInput>) -> Result<Vec<i64>> {
        let rows = rows.into_iter().map(NewPost::from).collect();
        block_on(ormcore_core::insert_many(&self.db, rows)).map_err(err)
    }

    #[napi]
    pub fn insert_post_sync(&self, row: NewPostInput) -> Result<i64> {
        block_on(ormcore_core::insert_one(&self.db, row.into())).map_err(err)
    }

    #[napi]
    pub fn delete_posts_above_sync(&self, id: i64) -> Result<i64> {
        block_on(ormcore_core::delete_above(&self.db, id))
            .map(|n| n as i64)
            .map_err(err)
    }
}

/// `const client = await connect(url, maxConnections)`
#[napi]
pub async fn connect(url: String, max_connections: Option<u32>) -> Result<Client> {
    let db = ormcore_core::connect(&url, max_connections.unwrap_or(1))
        .await
        .map_err(err)?;
    Ok(Client { db })
}
