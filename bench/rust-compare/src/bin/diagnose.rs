//! Isolate planning, runtime cell decoding, and streaming on the same read workload.
use futures_util::TryStreamExt;
use orm_core::{
    dialect::Target,
    ir::{ColType, Operation, ValueType},
    schema::Schema,
};
use orm_engine::{
    db::{Cell, Driver, RowSet},
    exec::Outcome,
    plan::Planner,
    NoParams,
};
use std::{hint::black_box, time::Instant};

const SQL: &str = "SELECT id, title, body, views FROM orm_rust_diagnose_post ORDER BY id LIMIT $1";
const ROUNDS: usize = 18;
const LABELS: [&str; 9] = [
    "our_planner",
    "our_driver",
    "tokio_buffered",
    "tokio_streamed",
    "diesel_async",
    "raw_sqlx_default",
    "raw_sqlx_fast",
    "seaorm_default",
    "seaorm_fast",
];

diesel::table! {
    orm_rust_diagnose_post (id) {
        id -> Integer,
        title -> Text,
        body -> Text,
        views -> Integer,
    }
}

mod sea_post {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, diesel::Queryable)]
    #[sea_orm(table_name = "orm_rust_diagnose_post")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i32,
        pub title: String,
        #[sea_orm(column_type = "Text")]
        pub body: String,
        pub views: i32,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
use sea_post::Model as Post;

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for Post {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get(0)?,
            title: row.try_get(1)?,
            body: row.try_get(2)?,
            views: row.try_get(3)?,
        })
    }
}

fn cells(rows: &dyn RowSet) -> Vec<Post> {
    let integer = ValueType::scalar(ColType::Int);
    let text = ValueType::scalar(ColType::Text);
    let mut result = Vec::with_capacity(rows.len());
    for i in 0..rows.len() {
        let Cell::Int(id) = rows.cell(i, 0, integer).unwrap() else {
            panic!("id")
        };
        let Cell::Text(title) = rows.cell(i, 1, text).unwrap() else {
            panic!("title")
        };
        let Cell::Text(body) = rows.cell(i, 2, text).unwrap() else {
            panic!("body")
        };
        let Cell::Int(views) = rows.cell(i, 3, integer).unwrap() else {
            panic!("views")
        };
        result.push(Post {
            id,
            title: title.into(),
            body: body.into(),
            views,
        });
    }
    result
}

fn typed(row: &tokio_postgres::Row) -> Post {
    Post {
        id: row.try_get(0).unwrap(),
        title: row.try_get(1).unwrap(),
        body: row.try_get(2).unwrap(),
        views: row.try_get(3).unwrap(),
    }
}

type DieselPool = diesel_async::pooled_connection::deadpool::Pool<diesel_async::AsyncPgConnection>;
struct Harness {
    ours: std::sync::Arc<dyn Driver>,
    schema: Schema,
    pg: deadpool_postgres::Pool,
    diesel: DieselPool,
    sqlx: sqlx::PgPool,
    sqlx_fast: sqlx::PgPool,
    sea_default: sea_orm::DatabaseConnection,
    sea_fast: sea_orm::DatabaseConnection,
}

impl Harness {
    async fn run(&self, contender: usize, n: i64, template: &Operation) -> Vec<Post> {
        match contender {
            0 => {
                let Operation::Select(q) = template else {
                    unreachable!()
                };
                let op = Operation::Select(q.clone());
                let target = Target::new(self.ours.dialect());
                let plan = Planner::plan(&self.schema, target, &op, &NoParams).unwrap();
                let Outcome::Select(s) = orm_engine::exec::run(self.ours.as_ref(), target, plan)
                    .await
                    .unwrap()
                else {
                    unreachable!()
                };
                cells(s.rows.as_ref())
            }
            1 => {
                let rows = self
                    .ours
                    .query(SQL.into(), vec![sea_query::Value::BigInt(Some(n))])
                    .await
                    .unwrap();
                cells(rows.as_ref())
            }
            2 | 3 => {
                let client = self.pg.get().await.unwrap();
                let statement = client
                    .prepare_typed_cached(SQL, &[tokio_postgres::types::Type::INT8])
                    .await
                    .unwrap();
                if contender == 2 {
                    let rows = client.query(&statement, &[&n]).await.unwrap();
                    let mut models = Vec::with_capacity(rows.len());
                    for row in &rows {
                        models.push(typed(row));
                    }
                    models
                } else {
                    let stream = client
                        .query_raw(
                            &statement,
                            std::iter::once(&n as &(dyn tokio_postgres::types::ToSql + Sync)),
                        )
                        .await
                        .unwrap();
                    futures_util::pin_mut!(stream);
                    let mut models = Vec::with_capacity(n as usize);
                    while let Some(row) = stream.try_next().await.unwrap() {
                        models.push(typed(&row));
                    }
                    models
                }
            }
            4 => {
                use diesel::{ExpressionMethods, QueryDsl};
                use diesel_async::RunQueryDsl;
                let mut connection = self.diesel.get().await.unwrap();
                orm_rust_diagnose_post::table
                    .order(orm_rust_diagnose_post::id.asc())
                    .limit(n)
                    .load::<Post>(&mut connection)
                    .await
                    .unwrap()
            }
            5 => sqlx::query_as::<_, Post>(SQL)
                .bind(n)
                .fetch_all(&self.sqlx)
                .await
                .unwrap(),
            6 => sqlx::query_as::<_, Post>(SQL)
                .bind(n)
                .fetch_all(&self.sqlx_fast)
                .await
                .unwrap(),
            7 | 8 => {
                use sea_orm::{EntityTrait, QueryOrder, QuerySelect};
                let pool = if contender == 7 {
                    &self.sea_default
                } else {
                    &self.sea_fast
                };
                sea_post::Entity::find()
                    .order_by_asc(sea_post::Column::Id)
                    .limit(n as u64)
                    .all(pool)
                    .await
                    .unwrap()
            }
            _ => unreachable!(),
        }
    }
}

fn stats(mut values: Vec<f64>) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    let median = if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    };
    serde_json::json!({"median_us":median,"min_batch_us":values[0],"max_batch_us":values[values.len()-1],"batches_us":values})
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url = std::env::var("ORM_BENCH_URL").expect("use a disposable PostgreSQL database");
    let ours = orm_engine::db::connect(&url, 1).await.unwrap();
    let config = url.parse::<tokio_postgres::Config>().unwrap();
    let manager = deadpool_postgres::Manager::from_config(
        config,
        tokio_postgres::NoTls,
        deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast,
        },
    );
    let pg = deadpool_postgres::Pool::builder(manager)
        .max_size(1)
        .build()
        .unwrap();
    let mut diesel_config = diesel_async::pooled_connection::ManagerConfig::default();
    diesel_config.recycling_method = diesel_async::pooled_connection::RecyclingMethod::Fast;
    let manager = diesel_async::pooled_connection::AsyncDieselConnectionManager::<
        diesel_async::AsyncPgConnection,
    >::new_with_config(url.clone(), diesel_config);
    let diesel = DieselPool::builder(manager).max_size(1).build().unwrap();
    let sqlx = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let sqlx_fast = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .test_before_acquire(false)
        .connect(&url)
        .await
        .unwrap();
    let mut options = sea_orm::ConnectOptions::new(url.clone());
    options
        .max_connections(1)
        .min_connections(1)
        .sqlx_logging(false);
    let sea_default = sea_orm::Database::connect(options.clone()).await.unwrap();
    options.test_before_acquire(false);
    let sea_fast = sea_orm::Database::connect(options).await.unwrap();
    ours.batch("CREATE TABLE orm_rust_diagnose_post (id integer PRIMARY KEY, title text NOT NULL, body text NOT NULL, views integer NOT NULL); INSERT INTO orm_rust_diagnose_post SELECT i, 'post ' || i, repeat('x', 200), i FROM generate_series(1, 1000) i; ANALYZE orm_rust_diagnose_post;".into()).await.unwrap();
    let ir = orm_core::dsl::compile("model Post {\n id Int @id\n title String\n body String @db.Text\n views Int\n @@map(\"orm_rust_diagnose_post\")\n}", None).unwrap();
    let harness = Harness {
        ours,
        schema: Schema::from_ir(ir).unwrap(),
        pg,
        diesel,
        sqlx,
        sqlx_fast,
        sea_default,
        sea_fast,
    };
    let mut results = vec![];
    for (n, inner) in [(1_i64, 200), (50, 100), (1000, 30)] {
        let op = orm_engine::parse_op(&serde_json::json!({"op":"select","model":"Post","order":[{"expr":{"t":"col","path":[],"name":"id"}}],"limit":n}).to_string()).unwrap();
        let expected: Vec<Post> = (1..=n as i32)
            .map(|id| Post {
                id,
                title: format!("post {id}"),
                body: "x".repeat(200),
                views: id,
            })
            .collect();
        for contender in 0..LABELS.len() {
            assert_eq!(harness.run(contender, n, &op).await, expected);
            for _ in 0..30 {
                black_box(harness.run(contender, n, &op).await);
            }
        }
        let mut batches: [Vec<f64>; 9] = std::array::from_fn(|_| vec![]);
        for round in 0..ROUNDS {
            for offset in 0..LABELS.len() {
                let contender = (round + offset) % LABELS.len();
                let start = Instant::now();
                for _ in 0..inner {
                    black_box(harness.run(contender, n, &op).await);
                }
                batches[contender].push(start.elapsed().as_secs_f64() * 1e6 / inner as f64);
            }
        }
        let mut summary = serde_json::Map::new();
        for (label, samples) in LABELS.iter().zip(batches) {
            let value = stats(samples);
            println!(
                "read {n:4} {label:16}: {:8.2} us",
                value["median_us"].as_f64().unwrap()
            );
            summary.insert(label.to_string(), value);
        }
        results
            .push(serde_json::json!({"rows":n,"iterations_per_batch":inner,"contenders":summary}));
    }
    // Reuse already fetched rows to isolate CPU-only decode/allocation/destruction.
    let dynamic_rows = harness
        .ours
        .query(SQL.into(), vec![sea_query::Value::BigInt(Some(1000))])
        .await
        .unwrap();
    let client = harness.pg.get().await.unwrap();
    let statement = client
        .prepare_typed_cached(SQL, &[tokio_postgres::types::Type::INT8])
        .await
        .unwrap();
    let raw_rows = client.query(&statement, &[&1000_i64]).await.unwrap();
    let decode = |kind: usize| {
        if kind == 0 {
            cells(dynamic_rows.as_ref())
        } else {
            let mut models = Vec::with_capacity(raw_rows.len());
            for row in &raw_rows {
                models.push(typed(row));
            }
            models
        }
    };
    assert_eq!(decode(0), decode(1));
    for _ in 0..30 {
        black_box(decode(0));
        black_box(decode(1));
    }
    let mut cpu_batches = [vec![], vec![]];
    for round in 0..18 {
        for offset in 0..2 {
            let kind = (round + offset) % 2;
            let start = Instant::now();
            for _ in 0..200 {
                black_box(decode(kind));
            }
            cpu_batches[kind].push(start.elapsed().as_secs_f64() * 1e6 / 200.0);
        }
    }
    let [dynamic, typed] = cpu_batches.map(stats);
    println!(
        "decode only 1000: dynamic {:.2} us | typed {:.2} us",
        dynamic["median_us"].as_f64().unwrap(),
        typed["median_us"].as_f64().unwrap()
    );
    harness
        .ours
        .batch("DROP TABLE orm_rust_diagnose_post".into())
        .await
        .unwrap();
    let version = harness
        .ours
        .query_text("SELECT version()".into())
        .await
        .unwrap();
    let output = std::env::var("ORM_BENCH_OUT").unwrap_or_else(|_| "results-diagnose.json".into());
    let report = serde_json::json!({"database":version,"rounds":ROUNDS,"pool_size":1,"cases":results,"cpu_decode_1000":{"dynamic_cells":dynamic,"typed_get":typed}});
    std::fs::write(
        output,
        serde_json::to_string_pretty(&report).unwrap() + "\n",
    )
    .unwrap();
    harness.sqlx.close().await;
    harness.sqlx_fast.close().await;
    harness.sea_default.close().await.unwrap();
    harness.sea_fast.close().await.unwrap();
    harness.diesel.close();
    harness.pg.close();
    harness.ours.close().await;
}
