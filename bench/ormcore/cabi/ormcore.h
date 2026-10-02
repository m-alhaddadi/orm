/* C ABI for ormcore-core. All calls block the calling thread on a shared Tokio runtime.
 * Functions returning int return 0 on success; on failure *err is set to a message the
 * caller must release with orm_free_error. */
#ifndef ORMCORE_H
#define ORMCORE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct OrmClient OrmClient;

/* Borrowed UTF-8 bytes (not NUL-terminated), valid until the owning list is freed. */
typedef struct {
    const uint8_t *ptr;
    size_t len;
} OrmStr;

typedef struct {
    int64_t id;
    int64_t author_id;
    OrmStr title;
    OrmStr body;
    int32_t views;
    bool published;
    int64_t created_at_us; /* Unix microseconds, UTC */
    bool has_author;       /* set when fetched with join */
    int64_t author_id_val;
    OrmStr author_name;
    OrmStr author_email;
    int64_t author_created_at_us;
} OrmPost;

typedef struct {
    const OrmPost *ptr;
    size_t len;
    void *owner; /* opaque; pass the list to orm_free_posts */
} OrmPostList;

/* A post to insert. String fields are offsets into a caller-provided byte arena, so the
 * rows contain no pointers (required by cgo's pointer-passing rules). */
typedef struct {
    int64_t author_id;
    size_t title_off;
    size_t title_len;
    size_t body_off;
    size_t body_len;
    int32_t views;
    bool published;
    int64_t created_at_us;
} OrmNewPost;

OrmClient *orm_connect(const char *url, uint32_t max_connections, char **err);
void orm_close(OrmClient *client);

int orm_fetch_posts(OrmClient *client, uint64_t limit, bool with_author, OrmPostList *out, char **err);
void orm_free_posts(OrmPostList *list);

int orm_insert_posts(OrmClient *client, const OrmNewPost *rows, size_t n, const uint8_t *arena,
                     size_t arena_len, int64_t *out_ids, char **err);
int orm_insert_post(OrmClient *client, const OrmNewPost *row, const uint8_t *arena, size_t arena_len,
                    int64_t *out_id, char **err);
int orm_delete_above(OrmClient *client, int64_t id, uint64_t *out_deleted, char **err);

void orm_noop(void);
void orm_free_error(char *err);

#endif
