//! C ABI over ormcore-core for Go (cgo). Every call blocks the calling OS thread on the
//! shared Tokio runtime. Fetched rows stay owned by Rust; the caller copies what it needs
//! and then calls `orm_free_posts`. See `ormcore.h` for the contract.

use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;

use chrono::{DateTime, FixedOffset};
use ormcore_core::{runtime, Db, NewPost, PostWithAuthor};

pub struct OrmClient {
    db: Db,
}

#[repr(C)]
pub struct OrmStr {
    ptr: *const u8,
    len: usize,
}

impl OrmStr {
    const EMPTY: OrmStr = OrmStr { ptr: ptr::null(), len: 0 };

    fn of(s: &str) -> Self {
        OrmStr { ptr: s.as_ptr(), len: s.len() }
    }
}

#[repr(C)]
pub struct OrmPost {
    id: i64,
    author_id: i64,
    title: OrmStr,
    body: OrmStr,
    views: i32,
    published: bool,
    created_at_us: i64,
    has_author: bool,
    author_id_val: i64,
    author_name: OrmStr,
    author_email: OrmStr,
    author_created_at_us: i64,
}

#[repr(C)]
pub struct OrmPostList {
    ptr: *const OrmPost,
    len: usize,
    owner: *mut c_void,
}

#[repr(C)]
pub struct OrmNewPost {
    author_id: i64,
    title_off: usize,
    title_len: usize,
    body_off: usize,
    body_len: usize,
    views: i32,
    published: bool,
    created_at_us: i64,
}

/// Keeps the models alive while Go reads the borrowed strings.
struct Fetched {
    _models: Vec<PostWithAuthor>,
    flat: Vec<OrmPost>,
}

fn set_err(err: *mut *mut c_char, msg: impl Into<Vec<u8>>) -> i32 {
    if !err.is_null() {
        let msg = CString::new(msg).unwrap_or_else(|_| CString::new("error").unwrap());
        unsafe { *err = msg.into_raw() };
    }
    -1
}

fn micros(dt: &DateTime<FixedOffset>) -> i64 {
    dt.timestamp_micros()
}

fn from_micros(us: i64) -> DateTime<FixedOffset> {
    DateTime::from_timestamp_micros(us).unwrap_or_default().fixed_offset()
}

unsafe fn arena_str(arena: &[u8], off: usize, len: usize) -> Result<String, String> {
    let bytes = arena.get(off..off + len).ok_or("string out of arena bounds")?;
    String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())
}

unsafe fn new_post(row: &OrmNewPost, arena: &[u8]) -> Result<NewPost, String> {
    Ok(NewPost {
        author_id: row.author_id,
        title: arena_str(arena, row.title_off, row.title_len)?,
        body: arena_str(arena, row.body_off, row.body_len)?,
        views: row.views,
        published: row.published,
        created_at: from_micros(row.created_at_us),
    })
}

unsafe fn arena<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len)
    }
}

#[no_mangle]
pub unsafe extern "C" fn orm_connect(url: *const c_char, max_connections: u32, err: *mut *mut c_char) -> *mut OrmClient {
    let url = match CStr::from_ptr(url).to_str() {
        Ok(u) => u,
        Err(e) => {
            set_err(err, e.to_string());
            return ptr::null_mut();
        }
    };
    match runtime().block_on(ormcore_core::connect(url, max_connections)) {
        Ok(db) => Box::into_raw(Box::new(OrmClient { db })),
        Err(e) => {
            set_err(err, e.to_string());
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn orm_close(client: *mut OrmClient) {
    if !client.is_null() {
        let client = Box::from_raw(client);
        let _ = runtime().block_on(client.db.close());
    }
}

#[no_mangle]
pub unsafe extern "C" fn orm_fetch_posts(
    client: *mut OrmClient,
    limit: u64,
    with_author: bool,
    out: *mut OrmPostList,
    err: *mut *mut c_char,
) -> i32 {
    let db = &(*client).db;
    let models = if with_author {
        runtime().block_on(ormcore_core::select_posts_with_author(db, limit))
    } else {
        runtime()
            .block_on(ormcore_core::select_posts(db, limit))
            .map(|v| v.into_iter().map(|p| (p, None)).collect())
    };
    let models = match models {
        Ok(m) => m,
        Err(e) => return set_err(err, e.to_string()),
    };
    let flat = models
        .iter()
        .map(|(p, a)| OrmPost {
            id: p.id,
            author_id: p.author_id,
            title: OrmStr::of(&p.title),
            body: OrmStr::of(&p.body),
            views: p.views,
            published: p.published,
            created_at_us: micros(&p.created_at),
            has_author: a.is_some(),
            author_id_val: a.as_ref().map_or(0, |a| a.id),
            author_name: a.as_ref().map_or(OrmStr::EMPTY, |a| OrmStr::of(&a.name)),
            author_email: a.as_ref().map_or(OrmStr::EMPTY, |a| OrmStr::of(&a.email)),
            author_created_at_us: a.as_ref().map_or(0, |a| micros(&a.created_at)),
        })
        .collect::<Vec<_>>();
    let fetched = Box::new(Fetched { _models: models, flat });
    *out = OrmPostList {
        ptr: fetched.flat.as_ptr(),
        len: fetched.flat.len(),
        owner: Box::into_raw(fetched) as *mut c_void,
    };
    0
}

#[no_mangle]
pub unsafe extern "C" fn orm_free_posts(list: *mut OrmPostList) {
    if !list.is_null() && !(*list).owner.is_null() {
        drop(Box::from_raw((*list).owner as *mut Fetched));
        (*list).owner = ptr::null_mut();
        (*list).ptr = ptr::null();
        (*list).len = 0;
    }
}

#[no_mangle]
pub unsafe extern "C" fn orm_insert_posts(
    client: *mut OrmClient,
    rows: *const OrmNewPost,
    n: usize,
    arena_ptr: *const u8,
    arena_len: usize,
    out_ids: *mut i64,
    err: *mut *mut c_char,
) -> i32 {
    let arena = arena(arena_ptr, arena_len);
    let rows = if n == 0 { &[][..] } else { std::slice::from_raw_parts(rows, n) };
    let rows = match rows.iter().map(|r| new_post(r, arena)).collect::<Result<Vec<_>, _>>() {
        Ok(r) => r,
        Err(e) => return set_err(err, e),
    };
    match runtime().block_on(ormcore_core::insert_many(&(*client).db, rows)) {
        Ok(ids) => {
            if !out_ids.is_null() {
                ptr::copy_nonoverlapping(ids.as_ptr(), out_ids, ids.len().min(n));
            }
            0
        }
        Err(e) => set_err(err, e.to_string()),
    }
}

#[no_mangle]
pub unsafe extern "C" fn orm_insert_post(
    client: *mut OrmClient,
    row: *const OrmNewPost,
    arena_ptr: *const u8,
    arena_len: usize,
    out_id: *mut i64,
    err: *mut *mut c_char,
) -> i32 {
    let row = match new_post(&*row, arena(arena_ptr, arena_len)) {
        Ok(r) => r,
        Err(e) => return set_err(err, e),
    };
    match runtime().block_on(ormcore_core::insert_one(&(*client).db, row)) {
        Ok(id) => {
            if !out_id.is_null() {
                *out_id = id;
            }
            0
        }
        Err(e) => set_err(err, e.to_string()),
    }
}

#[no_mangle]
pub unsafe extern "C" fn orm_delete_above(
    client: *mut OrmClient,
    id: i64,
    out_deleted: *mut u64,
    err: *mut *mut c_char,
) -> i32 {
    match runtime().block_on(ormcore_core::delete_above(&(*client).db, id)) {
        Ok(n) => {
            if !out_deleted.is_null() {
                *out_deleted = n;
            }
            0
        }
        Err(e) => set_err(err, e.to_string()),
    }
}

/// Does nothing: measures the bare cgo call cost.
#[no_mangle]
pub extern "C" fn orm_noop() {}

#[no_mangle]
pub unsafe extern "C" fn orm_free_error(err: *mut c_char) {
    if !err.is_null() {
        drop(CString::from_raw(err));
    }
}
