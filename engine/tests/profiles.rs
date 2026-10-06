use orm_core::dialect::Dialect;
use orm_engine::db;

#[test]
fn availability_matches_compiled_features() {
    assert_eq!(db::require_dialect(Dialect::Postgres).is_ok(), cfg!(feature = "postgres"));
    assert_eq!(db::require_dialect(Dialect::Sqlite).is_ok(), cfg!(feature = "sqlite"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for (enabled, url) in [(cfg!(feature = "postgres"), "postgres://unreachable"),
                           (cfg!(feature = "sqlite"), "sqlite://:memory:")] {
        if !enabled {
            let error = runtime.block_on(db::connect(url, 1)).err().unwrap();
            assert!(error.message.contains("not compiled"));
        }
    }
}
