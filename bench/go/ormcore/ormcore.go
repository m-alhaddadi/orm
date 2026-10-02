// Package ormcore is the Go binding for the Rust ORM core, over its C ABI (cgo).
//
// Each method is one cgo call that blocks the calling OS thread while Rust runs the
// query on its Tokio runtime. Rows come back as a C array that borrows Rust-owned
// strings; they are copied into Go structs in one pass and the array is freed.
package ormcore

/*
#cgo CFLAGS: -I${SRCDIR}/../../ormcore/cabi
#cgo LDFLAGS: ${SRCDIR}/../../ormcore/target/release/libormcore_cabi.a -lm -ldl -lpthread
#include <stdlib.h>
#include "ormcore.h"
*/
import "C"

import (
	"errors"
	"time"
	"unsafe"
)

type Author struct {
	ID        int64
	Name      string
	Email     string
	CreatedAt time.Time
}

type Post struct {
	ID        int64
	AuthorID  int64
	Title     string
	Body      string
	Views     int32
	Published bool
	CreatedAt time.Time
	Author    *Author
}

type NewPost struct {
	AuthorID  int64
	Title     string
	Body      string
	Views     int32
	Published bool
	CreatedAt time.Time
}

type Client struct{ c *C.OrmClient }

func takeErr(e *C.char) error {
	msg := C.GoString(e)
	C.orm_free_error(e)
	return errors.New(msg)
}

func goStr(s C.OrmStr) string {
	if s.len == 0 {
		return ""
	}
	return C.GoStringN((*C.char)(unsafe.Pointer(s.ptr)), C.int(s.len))
}

func goTime(us C.int64_t) time.Time { return time.UnixMicro(int64(us)).UTC() }

func Connect(url string, maxConnections uint32) (*Client, error) {
	cu := C.CString(url)
	defer C.free(unsafe.Pointer(cu))
	var e *C.char
	c := C.orm_connect(cu, C.uint32_t(maxConnections), &e)
	if c == nil {
		return nil, takeErr(e)
	}
	return &Client{c}, nil
}

func (cl *Client) Close() { C.orm_close(cl.c) }

func (cl *Client) fetch(limit int, withAuthor bool) ([]Post, error) {
	var list C.OrmPostList
	var e *C.char
	if C.orm_fetch_posts(cl.c, C.uint64_t(limit), C.bool(withAuthor), &list, &e) != 0 {
		return nil, takeErr(e)
	}
	defer C.orm_free_posts(&list)
	rows := unsafe.Slice(list.ptr, int(list.len))
	out := make([]Post, len(rows))
	var authors []Author
	if withAuthor {
		authors = make([]Author, len(rows))
	}
	for i := range rows {
		r := &rows[i]
		out[i] = Post{
			ID:        int64(r.id),
			AuthorID:  int64(r.author_id),
			Title:     goStr(r.title),
			Body:      goStr(r.body),
			Views:     int32(r.views),
			Published: bool(r.published),
			CreatedAt: goTime(r.created_at_us),
		}
		if r.has_author {
			authors[i] = Author{
				ID:        int64(r.author_id_val),
				Name:      goStr(r.author_name),
				Email:     goStr(r.author_email),
				CreatedAt: goTime(r.author_created_at_us),
			}
			out[i].Author = &authors[i]
		}
	}
	return out, nil
}

func (cl *Client) FetchPosts(limit int) ([]Post, error)           { return cl.fetch(limit, false) }
func (cl *Client) FetchPostsWithAuthor(limit int) ([]Post, error) { return cl.fetch(limit, true) }

// pack lays rows out as pointer-free C structs plus one byte arena for the strings,
// which is what cgo allows passing without copying.
func pack(rows []NewPost) ([]C.OrmNewPost, []byte) {
	size := 0
	for i := range rows {
		size += len(rows[i].Title) + len(rows[i].Body)
	}
	arena := make([]byte, 0, size+1)
	crows := make([]C.OrmNewPost, len(rows))
	for i := range rows {
		r := &rows[i]
		crows[i] = C.OrmNewPost{
			author_id:     C.int64_t(r.AuthorID),
			title_off:     C.size_t(len(arena)),
			title_len:     C.size_t(len(r.Title)),
			views:         C.int32_t(r.Views),
			published:     C.bool(r.Published),
			created_at_us: C.int64_t(r.CreatedAt.UnixMicro()),
		}
		arena = append(arena, r.Title...)
		crows[i].body_off = C.size_t(len(arena))
		crows[i].body_len = C.size_t(len(r.Body))
		arena = append(arena, r.Body...)
	}
	return crows, append(arena, 0) // never empty, so &arena[0] is valid
}

// InsertPosts runs one INSERT ... RETURNING id for all rows.
func (cl *Client) InsertPosts(rows []NewPost) ([]int64, error) {
	if len(rows) == 0 {
		return nil, nil
	}
	crows, arena := pack(rows)
	ids := make([]int64, len(rows))
	var e *C.char
	if C.orm_insert_posts(cl.c, &crows[0], C.size_t(len(crows)), (*C.uint8_t)(&arena[0]),
		C.size_t(len(arena)), (*C.int64_t)(&ids[0]), &e) != 0 {
		return nil, takeErr(e)
	}
	return ids, nil
}

func (cl *Client) InsertPost(row NewPost) (int64, error) {
	crows, arena := pack([]NewPost{row})
	var id C.int64_t
	var e *C.char
	if C.orm_insert_post(cl.c, &crows[0], (*C.uint8_t)(&arena[0]), C.size_t(len(arena)), &id, &e) != 0 {
		return 0, takeErr(e)
	}
	return int64(id), nil
}

func (cl *Client) DeletePostsAbove(id int64) (uint64, error) {
	var n C.uint64_t
	var e *C.char
	if C.orm_delete_above(cl.c, C.int64_t(id), &n, &e) != 0 {
		return 0, takeErr(e)
	}
	return uint64(n), nil
}

// Noop is an empty cgo call, for measuring the bare call cost.
func Noop() { C.orm_noop() }
