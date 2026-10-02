// Phase 0 Go benchmark: GORM and raw pgx vs the Rust core over cgo.
//
//	gorm          GORM 1.31 on pgx (defaults: each Create runs in its own transaction)
//	pgx           raw pgx v5 queries scanned into structs, as a pure-Go floor
//	ormcore-cgo   Rust core (SeaORM) through the C ABI, rows copied into Go structs
//
// Same ops, sizes, iteration counts and field-touching rules as bench/run_bench.py.
//
//	go run . [-transport unix|tcp] [-quick] [-only gorm,pgx]
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"net/url"
	"os"
	"runtime"
	"slices"
	"sort"
	"strings"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"gorm.io/driver/postgres"
	"gorm.io/gorm"
	"gorm.io/gorm/logger"

	"ormbench/ormcore"
)

const seedMaxID = 1000

var (
	sizes = []int{1, 50, 1000}
	ops   = []string{"read", "read_join", "write_bulk", "write_loop"}
	iters = map[string]map[int]int{
		"read":       {1: 1000, 50: 300, 1000: 50},
		"read_join":  {1: 1000, 50: 300, 1000: 50},
		"write_bulk": {1: 500, 50: 200, 1000: 30},
		"write_loop": {1: 500, 50: 20, 1000: 3},
	}
)

// --- GORM models (tables created by Django) ---------------------------------------

type Author struct {
	ID        int64
	Name      string
	Email     string
	CreatedAt time.Time
}

func (Author) TableName() string { return "blog_author" }

type Post struct {
	ID        int64
	AuthorID  int64
	Author    *Author
	Title     string
	Body      string
	Views     int32
	Published bool
	CreatedAt time.Time
}

func (Post) TableName() string { return "blog_post" }

// --- field touching ---------------------------------------------------------------

var sink int64

func touchPost(id, authorID int64, title, body string, views int32, published bool, created time.Time) int64 {
	s := id + authorID + int64(views) + int64(len(title)) + int64(len(body)) + created.UnixNano()
	if published {
		s++
	}
	return s
}

func touchAuthor(id int64, name, email string, created time.Time) int64 {
	return id + int64(len(name)) + int64(len(email)) + created.UnixNano()
}

func touchGorm(rows []Post, join bool) {
	for i := range rows {
		p := &rows[i]
		sink += touchPost(p.ID, p.AuthorID, p.Title, p.Body, p.Views, p.Published, p.CreatedAt)
		if join {
			sink += touchAuthor(p.Author.ID, p.Author.Name, p.Author.Email, p.Author.CreatedAt)
		}
	}
}

func touchCore(rows []ormcore.Post, join bool) {
	for i := range rows {
		p := &rows[i]
		sink += touchPost(p.ID, p.AuthorID, p.Title, p.Body, p.Views, p.Published, p.CreatedAt)
		if join {
			sink += touchAuthor(p.Author.ID, p.Author.Name, p.Author.Email, p.Author.CreatedAt)
		}
	}
}

// --- inputs -----------------------------------------------------------------------

type rowInput struct {
	AuthorID  int64
	Title     string
	Body      string
	Views     int32
	Published bool
	CreatedAt time.Time
}

func makeRows(n int) []rowInput {
	now := time.Now().UTC()
	body := strings.Repeat("Lorem ipsum dolor sit amet, consectetur adipiscing elit. ", 4)
	rows := make([]rowInput, n)
	for i := range rows {
		rows[i] = rowInput{int64(i%50 + 1), fmt.Sprintf("Bench post %d", i), body, int32(i), i%2 == 0, now}
	}
	return rows
}

// --- contenders -------------------------------------------------------------------

type opFn func(n int, rows []rowInput) error

type contender struct {
	name string
	ops  map[string]opFn
}

func gormContender(db *gorm.DB) contender {
	toModels := func(rows []rowInput) []Post {
		out := make([]Post, len(rows))
		for i, r := range rows {
			out[i] = Post{AuthorID: r.AuthorID, Title: r.Title, Body: r.Body, Views: r.Views, Published: r.Published, CreatedAt: r.CreatedAt}
		}
		return out
	}
	return contender{"gorm", map[string]opFn{
		"read": func(n int, _ []rowInput) error {
			var posts []Post
			err := db.Order("id").Limit(n).Find(&posts).Error
			touchGorm(posts, false)
			return err
		},
		"read_join": func(n int, _ []rowInput) error {
			var posts []Post
			err := db.Joins("Author").Order("blog_post.id").Limit(n).Find(&posts).Error
			touchGorm(posts, true)
			return err
		},
		"write_bulk": func(n int, rows []rowInput) error {
			posts := toModels(rows)
			return db.Create(&posts).Error
		},
		"write_loop": func(n int, rows []rowInput) error {
			for _, p := range toModels(rows) {
				if err := db.Create(&p).Error; err != nil {
					return err
				}
			}
			return nil
		},
	}}
}

func pgxContender(pool *pgxpool.Pool) contender {
	ctx := context.Background()
	const cols = "p.id, p.author_id, p.title, p.body, p.views, p.published, p.created_at"
	read := func(n int, join bool) error {
		q := "SELECT " + cols + " FROM blog_post p ORDER BY p.id LIMIT $1"
		if join {
			q = "SELECT " + cols + ", a.id, a.name, a.email, a.created_at FROM blog_post p " +
				"LEFT JOIN blog_author a ON a.id = p.author_id ORDER BY p.id LIMIT $1"
		}
		rows, err := pool.Query(ctx, q, n)
		if err != nil {
			return err
		}
		defer rows.Close()
		posts := make([]Post, 0, n)
		for rows.Next() {
			var p Post
			dest := []any{&p.ID, &p.AuthorID, &p.Title, &p.Body, &p.Views, &p.Published, &p.CreatedAt}
			if join {
				p.Author = &Author{}
				dest = append(dest, &p.Author.ID, &p.Author.Name, &p.Author.Email, &p.Author.CreatedAt)
			}
			if err := rows.Scan(dest...); err != nil {
				return err
			}
			posts = append(posts, p)
		}
		touchGorm(posts, join)
		return rows.Err()
	}
	const insertCols = "(author_id, title, body, views, published, created_at)"
	return contender{"pgx", map[string]opFn{
		"read":      func(n int, _ []rowInput) error { return read(n, false) },
		"read_join": func(n int, _ []rowInput) error { return read(n, true) },
		"write_bulk": func(n int, rows []rowInput) error {
			a, t, b, v, p, c := make([]int64, n), make([]string, n), make([]string, n), make([]int32, n), make([]bool, n), make([]time.Time, n)
			for i, r := range rows {
				a[i], t[i], b[i], v[i], p[i], c[i] = r.AuthorID, r.Title, r.Body, r.Views, r.Published, r.CreatedAt
			}
			res, err := pool.Query(ctx, "INSERT INTO blog_post "+insertCols+
				" SELECT * FROM unnest($1::bigint[], $2::text[], $3::text[], $4::int[], $5::bool[], $6::timestamptz[]) RETURNING id",
				a, t, b, v, p, c)
			if err != nil {
				return err
			}
			for res.Next() {
			}
			res.Close()
			return res.Err()
		},
		"write_loop": func(n int, rows []rowInput) error {
			for _, r := range rows {
				var id int64
				if err := pool.QueryRow(ctx, "INSERT INTO blog_post "+insertCols+" VALUES ($1,$2,$3,$4,$5,$6) RETURNING id",
					r.AuthorID, r.Title, r.Body, r.Views, r.Published, r.CreatedAt).Scan(&id); err != nil {
					return err
				}
			}
			return nil
		},
	}}
}

func coreContender(c *ormcore.Client) contender {
	toNew := func(rows []rowInput) []ormcore.NewPost {
		out := make([]ormcore.NewPost, len(rows))
		for i, r := range rows {
			out[i] = ormcore.NewPost(r)
		}
		return out
	}
	return contender{"ormcore-cgo", map[string]opFn{
		"read": func(n int, _ []rowInput) error {
			posts, err := c.FetchPosts(n)
			touchCore(posts, false)
			return err
		},
		"read_join": func(n int, _ []rowInput) error {
			posts, err := c.FetchPostsWithAuthor(n)
			touchCore(posts, true)
			return err
		},
		"write_bulk": func(n int, rows []rowInput) error {
			_, err := c.InsertPosts(toNew(rows))
			return err
		},
		"write_loop": func(n int, rows []rowInput) error {
			for _, r := range toNew(rows) {
				if _, err := c.InsertPost(r); err != nil {
					return err
				}
			}
			return nil
		},
	}}
}

// --- harness ----------------------------------------------------------------------

type result struct {
	Contender string  `json:"contender"`
	Op        string  `json:"op"`
	N         int     `json:"n"`
	Iters     int     `json:"iters"`
	MedianUs  float64 `json:"median_us"`
	P95Us     float64 `json:"p95_us"`
	MeanUs    float64 `json:"mean_us"`
	UsPerRow  float64 `json:"us_per_row"`
}

func summarize(name, op string, n int, s []int64) result {
	sort.Slice(s, func(i, j int) bool { return s[i] < s[j] })
	var med float64
	if len(s)%2 == 1 {
		med = float64(s[len(s)/2])
	} else {
		med = float64(s[len(s)/2-1]+s[len(s)/2]) / 2
	}
	var sum int64
	for _, v := range s {
		sum += v
	}
	return result{name, op, n, len(s), med / 1e3, float64(s[min(len(s)-1, len(s)*95/100)]) / 1e3,
		float64(sum) / float64(len(s)) / 1e3, med / 1e3 / float64(n)}
}

func must(err error) {
	if err != nil {
		panic(err)
	}
}

func main() {
	transport := flag.String("transport", "unix", "unix or tcp")
	quick := flag.Bool("quick", false, "10% of the iterations")
	only := flag.String("only", "", "comma-separated contenders")
	out := flag.String("out", "", "output JSON path")
	flag.Parse()
	if *out == "" {
		*out = fmt.Sprintf("../results-go-%s.json", *transport)
	}

	user, pass, dbname := "postgres", "postgres", "ormbench"
	var dsn, sqlxURL string
	if *transport == "tcp" {
		dsn = fmt.Sprintf("host=localhost port=5432 user=%s password=%s dbname=%s sslmode=disable", user, pass, dbname)
		sqlxURL = fmt.Sprintf("postgres://%s:%s@localhost:5432/%s", user, pass, dbname)
	} else {
		dsn = fmt.Sprintf("host=/var/run/postgresql port=5432 user=%s password=%s dbname=%s sslmode=disable", user, pass, dbname)
		sqlxURL = fmt.Sprintf("postgres://%s:%s@%s:5432/%s", user, pass, url.QueryEscape("/var/run/postgresql"), dbname)
	}

	gdb, err := gorm.Open(postgres.Open(dsn), &gorm.Config{Logger: logger.Discard})
	must(err)
	sqlDB, _ := gdb.DB()
	sqlDB.SetMaxOpenConns(1)
	sqlDB.SetMaxIdleConns(1)

	pcfg, err := pgxpool.ParseConfig(dsn)
	must(err)
	pcfg.MaxConns, pcfg.MinConns = 1, 1
	pool, err := pgxpool.NewWithConfig(context.Background(), pcfg)
	must(err)

	core, err := ormcore.Connect(sqlxURL, 1)
	must(err)
	admin, err := ormcore.Connect(sqlxURL, 1)
	must(err)

	contenders := []contender{gormContender(gdb), pgxContender(pool), coreContender(core)}
	var results []result
	for _, c := range contenders {
		if *only != "" && !slices.Contains(strings.Split(*only, ","), c.name) {
			continue
		}
		for _, op := range ops {
			_, err := admin.DeletePostsAbove(seedMaxID)
			must(err)
			_, err = pool.Exec(context.Background(), "VACUUM ANALYZE blog_post")
			must(err)
			for _, n := range sizes {
				it := iters[op][n]
				if *quick {
					it = max(3, it/10)
				}
				warm := max(3, it/10)
				samples := make([]int64, 0, it)
				for i := 0; i < warm+it; i++ {
					var rows []rowInput
					if strings.HasPrefix(op, "write") {
						rows = makeRows(n)
					}
					t0 := time.Now()
					must(c.ops[op](n, rows))
					dt := time.Since(t0).Nanoseconds()
					if strings.HasPrefix(op, "write") {
						_, err := admin.DeletePostsAbove(seedMaxID)
						must(err)
					}
					if i >= warm {
						samples = append(samples, dt)
					}
				}
				r := summarize(c.name, op, n, samples)
				results = append(results, r)
				fmt.Printf("%-12s %-11s n=%-5d median=%10.1fus p95=%10.1fus  %8.2fus/row\n",
					r.Contender, r.Op, r.N, r.MedianUs, r.P95Us, r.UsPerRow)
			}
		}
	}

	// Bare cgo call cost.
	const nCalls = 2_000_000
	t0 := time.Now()
	for i := 0; i < nCalls; i++ {
		ormcore.Noop()
	}
	cgoNs := float64(time.Since(t0).Nanoseconds()) / nCalls
	fmt.Printf("empty cgo call: %.1f ns\n", cgoNs)

	var pgVersion string
	must(pool.QueryRow(context.Background(), "SHOW server_version").Scan(&pgVersion))
	_, _ = admin.DeletePostsAbove(seedMaxID)

	env := map[string]any{
		"runtime":     runtime.Version(),
		"gorm":        "1.31",
		"postgres":    pgVersion,
		"cpus":        runtime.NumCPU(),
		"platform":    runtime.GOOS + "/" + runtime.GOARCH,
		"transport":   *transport,
		"baseline":    "gorm",
		"cgo_call_ns": cgoNs,
	}
	data, _ := json.MarshalIndent(map[string]any{"env": env, "results": results}, "", "  ")
	must(os.WriteFile(*out, data, 0o644))
	fmt.Println("\nwrote", *out)
}
