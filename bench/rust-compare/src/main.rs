//! Run only against a disposable database: this creates and drops its own table.
use orm_core::{
    dialect::Target,
    ir::{Operation, ValueType},
    schema::Schema,
};
use orm_engine::{
    db::{Cell, Driver},
    exec::{self, Outcome},
    plan::Planner,
    Params,
};
use sea_orm::{
    ColumnTrait, ConnectOptions, Database, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect,
};
use std::{hint::black_box, time::Instant};

const CONTENDERS: usize = 4;
const ROUNDS: usize = 16;

mod diesel_schema {
    diesel::table! {
        orm_rust_compare_post (id) {
            id -> Integer,
            title -> Text,
            body -> Text,
            views -> Integer,
        }
    }
}

mod post {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, diesel::Queryable)]
    #[sea_orm(table_name = "orm_rust_compare_post")]
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

struct IntParams(Vec<i32>);
impl Params for IntParams {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn value(&self, i: usize, _: Option<ValueType>) -> orm_engine::Result<sea_query::Value> {
        Ok(self.0[i].into())
    }
    fn text(&self, _: usize) -> orm_engine::Result<String> {
        unreachable!()
    }
    fn count(&self, i: usize) -> orm_engine::Result<u64> {
        Ok(self.0[i] as u64)
    }
}

struct Case {
    name: &'static str,
    json: String,
    op: Operation,
    params: IntParams,
    inner: usize,
    kind: usize,
    limit: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum ResultValue {
    Rows(Vec<post::Model>),
    Count(u64),
    Affected(u64),
}

async fn ours(db: &dyn Driver, schema: &Schema, c: &Case, json: bool) -> ResultValue {
    // Direct Rust constructs owned operations; cloning the template models those allocations.
    let op = if json {
        orm_engine::parse_op(&c.json).unwrap()
    } else {
        match &c.op {
            Operation::Select(q) => Operation::Select(q.clone()),
            Operation::Count(q) => Operation::Count(q.clone()),
            Operation::Update(q) => Operation::Update(orm_core::ir::Update {
                model_fields: q.model_fields.clone(),
                without_defaults: q.without_defaults,
                model: q.model.clone(),
                with: q.with.clone(),
                filters: q.filters.clone(),
                set: q
                    .set
                    .iter()
                    .map(|a| orm_core::ir::Assignment {
                        field: a.field.clone(),
                        value: a.value.clone(),
                    })
                    .collect(),
                returning: q.returning,
            }),
            _ => unreachable!(),
        }
    };
    let target = Target::new(db.dialect());
    let plan = Planner::plan(schema, target, &op, &c.params).unwrap();
    match exec::run(db, target, plan).await.unwrap() {
        Outcome::Select(s) => {
            let mut rows = Vec::with_capacity(s.rows.len());
            for i in 0..s.rows.len() {
                let Cell::Int(id) = s.rows.cell(i, 0, s.plan.types[0]).unwrap() else {
                    panic!("id")
                };
                let Cell::Text(title) = s.rows.cell(i, 1, s.plan.types[1]).unwrap() else {
                    panic!("title")
                };
                let Cell::Text(body) = s.rows.cell(i, 2, s.plan.types[2]).unwrap() else {
                    panic!("body")
                };
                let Cell::Int(views) = s.rows.cell(i, 3, s.plan.types[3]).unwrap() else {
                    panic!("views")
                };
                rows.push(post::Model {
                    id,
                    title: title.into(),
                    body: body.into(),
                    views,
                });
            }
            ResultValue::Rows(rows)
        }
        Outcome::Count(n) => ResultValue::Count(n as u64),
        Outcome::Affected(n) => ResultValue::Affected(n),
        _ => unreachable!(),
    }
}

async fn sea(db: &DatabaseConnection, c: &Case) -> ResultValue {
    match c.kind {
        0 => ResultValue::Rows(
            post::Entity::find()
                .filter(post::Column::Id.eq(500))
                .all(db)
                .await
                .unwrap(),
        ),
        1 => ResultValue::Rows(
            post::Entity::find()
                .order_by_asc(post::Column::Id)
                .limit(c.limit)
                .all(db)
                .await
                .unwrap(),
        ),
        2 => ResultValue::Count(
            post::Entity::find()
                .filter(post::Column::Views.gt(990))
                .count(db)
                .await
                .unwrap(),
        ),
        3 => ResultValue::Affected(
            post::Entity::update_many()
                .col_expr(post::Column::Views, sea_orm::sea_query::Expr::value(0))
                .filter(post::Column::Id.lte(100))
                .exec(db)
                .await
                .unwrap()
                .rows_affected,
        ),
        _ => unreachable!(),
    }
}

type DieselPool = diesel_async::pooled_connection::deadpool::Pool<diesel_async::AsyncPgConnection>;

async fn diesel_run(pool: &DieselPool, c: &Case) -> ResultValue {
    use diesel::{ExpressionMethods, QueryDsl};
    use diesel_async::RunQueryDsl;
    use diesel_schema::orm_rust_compare_post::dsl::*;

    let mut conn = pool.get().await.unwrap();
    match c.kind {
        0 => ResultValue::Rows(
            orm_rust_compare_post
                .filter(id.eq(500))
                .load::<post::Model>(&mut conn)
                .await
                .unwrap(),
        ),
        1 => ResultValue::Rows(
            orm_rust_compare_post
                .order(id.asc())
                .limit(c.limit as i64)
                .load::<post::Model>(&mut conn)
                .await
                .unwrap(),
        ),
        2 => ResultValue::Count(
            orm_rust_compare_post
                .filter(views.gt(990))
                .count()
                .get_result::<i64>(&mut conn)
                .await
                .unwrap() as u64,
        ),
        3 => ResultValue::Affected(
            diesel::update(orm_rust_compare_post.filter(id.le(100)))
                .set(views.eq(0))
                .execute(&mut conn)
                .await
                .unwrap() as u64,
        ),
        _ => unreachable!(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url = std::env::var("ORM_BENCH_URL")
        .expect("set ORM_BENCH_URL to a disposable PostgreSQL database");
    let db = orm_engine::db::connect(&url, 1).await.unwrap();
    let mut config = diesel_async::pooled_connection::ManagerConfig::default();
    config.recycling_method = diesel_async::pooled_connection::RecyclingMethod::Fast;
    let manager = diesel_async::pooled_connection::AsyncDieselConnectionManager::<
        diesel_async::AsyncPgConnection,
    >::new_with_config(url.clone(), config);
    let diesel_pool = DieselPool::builder(manager).max_size(1).build().unwrap();
    let mut opts = ConnectOptions::new(url);
    opts.max_connections(1)
        .min_connections(1)
        .sqlx_logging(false);
    let sea_fast = std::env::var("ORM_BENCH_SEA_FAST").is_ok_and(|value| value == "1");
    opts.test_before_acquire(!sea_fast);
    let sea_db = Database::connect(opts).await.unwrap();
    db.batch("CREATE TABLE orm_rust_compare_post (id integer PRIMARY KEY, title text NOT NULL, body text NOT NULL, views integer NOT NULL); INSERT INTO orm_rust_compare_post SELECT i, 'post ' || i, repeat('x', 200), i FROM generate_series(1, 1000) i; ANALYZE orm_rust_compare_post;".into()).await.unwrap();
    let ir = orm_core::dsl::compile("model Post {\n id Int @id\n title String\n body String @db.Text\n views Int\n @@map(\"orm_rust_compare_post\")\n}", None).unwrap();
    let schema = Schema::from_ir(ir).unwrap();
    let col = |name: &str| serde_json::json!({"t":"col", "path":[], "name":name});
    let cmp = |name: &str, op: &str, i: usize| serde_json::json!({"t":"cmp", "op":op, "l":col(name), "r":{"t":"param", "i":i}});
    let specs = [
        (
            "get by pk",
            serde_json::json!({"op":"select","model":"Post","filters":[cmp("id","eq",0)]}),
            vec![500],
            150,
            0,
            1,
        ),
        (
            "read 50",
            serde_json::json!({"op":"select","model":"Post","order":[{"expr":col("id")}],"limit":50}),
            vec![],
            100,
            1,
            50,
        ),
        (
            "read 1000",
            serde_json::json!({"op":"select","model":"Post","order":[{"expr":col("id")}],"limit":1000}),
            vec![],
            20,
            1,
            1000,
        ),
        (
            "filtered count",
            serde_json::json!({"op":"count","model":"Post","filters":[cmp("views","gt",0)]}),
            vec![990],
            150,
            2,
            0,
        ),
        (
            "update 100",
            serde_json::json!({"op":"update","model":"Post","filters":[cmp("id","le",1)],"set":[{"field":"views","value":{"t":"param","i":0}}]}),
            vec![0, 100],
            50,
            3,
            0,
        ),
    ];
    let mut results = vec![];
    for (name, doc, params, inner, kind, limit) in specs {
        let json = doc.to_string();
        let c = Case {
            name,
            op: orm_engine::parse_op(&json).unwrap(),
            json,
            params: IntParams(params),
            inner,
            kind,
            limit,
        };
        let reference = sea(&sea_db, &c).await;
        assert_eq!(reference, ours(db.as_ref(), &schema, &c, false).await);
        assert_eq!(reference, ours(db.as_ref(), &schema, &c, true).await);
        assert_eq!(reference, diesel_run(&diesel_pool, &c).await);
        if let ResultValue::Rows(rows) = &reference {
            assert_eq!(rows.len() as u64, c.limit);
            for (index, row) in rows.iter().enumerate() {
                let expected_id = if c.kind == 0 { 500 } else { index as i32 + 1 };
                assert_eq!(row.id, expected_id);
                assert_eq!(row.views, expected_id);
                assert_eq!(row.title, format!("post {expected_id}"));
                assert_eq!(row.body, "x".repeat(200));
            }
        }
        if c.kind == 2 {
            assert_eq!(reference, ResultValue::Count(10));
        }
        if c.kind == 3 {
            assert_eq!(reference, ResultValue::Affected(100));
            // Check each implementation's database changes from the original state.
            // These resets and assertions are outside every timed batch.
            for contender in 0..CONTENDERS {
                db.batch("UPDATE orm_rust_compare_post SET views = id".into())
                    .await
                    .unwrap();
                let affected = match contender {
                    0 => sea(&sea_db, &c).await,
                    1 => ours(db.as_ref(), &schema, &c, false).await,
                    2 => ours(db.as_ref(), &schema, &c, true).await,
                    _ => diesel_run(&diesel_pool, &c).await,
                };
                assert_eq!(affected, ResultValue::Affected(100));
                let wrong = db.query_text("SELECT count(*) FROM orm_rust_compare_post WHERE (id <= 100 AND views <> 0) OR (id > 100 AND views <> id)".into()).await.unwrap();
                assert_eq!(wrong, vec![vec![Some("0".to_owned())]]);
            }
        }
        for _ in 0..30 {
            black_box(sea(&sea_db, &c).await);
            black_box(ours(db.as_ref(), &schema, &c, false).await);
            black_box(ours(db.as_ref(), &schema, &c, true).await);
            black_box(diesel_run(&diesel_pool, &c).await);
        }
        db.batch("VACUUM ANALYZE orm_rust_compare_post".into())
            .await
            .unwrap();
        let mut samples: [Vec<f64>; CONTENDERS] = std::array::from_fn(|_| vec![]);
        // Rotate execution order each round to reduce drift bias.
        for round in 0..ROUNDS {
            for offset in 0..CONTENDERS {
                let contender = (round + offset) % CONTENDERS;
                let start = Instant::now();
                for _ in 0..c.inner {
                    black_box(match contender {
                        0 => sea(&sea_db, &c).await,
                        1 => ours(db.as_ref(), &schema, &c, false).await,
                        2 => ours(db.as_ref(), &schema, &c, true).await,
                        _ => diesel_run(&diesel_pool, &c).await,
                    });
                }
                samples[contender].push(start.elapsed().as_secs_f64() * 1e6 / c.inner as f64);
            }
        }
        let mut stats = vec![];
        for values in &mut samples {
            values.sort_by(f64::total_cmp);
            let mid = values.len() / 2;
            let median = (values[mid - 1] + values[mid]) / 2.0;
            stats.push(serde_json::json!({"median_us":median, "min_batch_us":values[0], "max_batch_us":values[values.len() - 1], "batches_us":values}));
        }
        println!(
            "{:16} SeaORM {:8.2} us | direct {:8.2} us | JSON {:8.2} us | Diesel {:8.2} us",
            c.name,
            stats[0]["median_us"].as_f64().unwrap(),
            stats[1]["median_us"].as_f64().unwrap(),
            stats[2]["median_us"].as_f64().unwrap(),
            stats[3]["median_us"].as_f64().unwrap()
        );
        results.push(serde_json::json!({"case":c.name,"iterations_per_batch":c.inner,"seaorm":stats[0],"direct_ir":stats[1],"json_ir":stats[2],"diesel_async":stats[3]}));
    }
    db.batch("DROP TABLE orm_rust_compare_post".into())
        .await
        .unwrap();
    let version = db.query_text("SELECT version()".into()).await.unwrap();
    let output = std::env::var("ORM_BENCH_OUT").unwrap_or_else(|_| "results-diesel.json".into());
    std::fs::write(output, serde_json::to_string_pretty(&serde_json::json!({"database":version,"seaorm_version":"2.0.4","diesel_version":"2.3.13","diesel_async_version":"0.9.2","seaorm_test_before_acquire":!sea_fast,"rounds":ROUNDS,"pool_size":1,"cases":results})).unwrap() + "\n").unwrap();
    diesel_pool.close();
    sea_db.close().await.unwrap();
    db.close().await;
}
