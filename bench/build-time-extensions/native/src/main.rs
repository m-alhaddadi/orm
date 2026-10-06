//! Allocation evidence only; timing uses uninstrumented release language bindings.
#[cfg(feature = "allocation-probe")]
use std::alloc::{GlobalAlloc, Layout, System};
#[cfg(feature = "allocation-probe")]
use std::sync::atomic::{AtomicUsize, Ordering};
use orm_core::{dialect::Target, dsl};
use orm_engine::{NoParams, parse_op, plan::Planner};

#[cfg(feature = "allocation-probe")]
struct Count;
#[cfg(feature = "allocation-probe")]
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "allocation-probe")]
static BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "allocation-probe")]
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) { System.dealloc(ptr, layout) }
    unsafe fn realloc(&self, ptr: *mut u8, old: Layout, size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size, Ordering::Relaxed);
        System.realloc(ptr, old, size)
    }
}
#[cfg(feature = "allocation-probe")]
#[global_allocator]
static ALLOCATOR: Count = Count;

fn main() {
    let n: usize = std::env::args().nth(2).map(|v|v.parse().unwrap()).unwrap_or(10000);
    let provider = std::env::args().nth(1).unwrap_or("postgresql".into());
    let source = "model BenchItem {\n id BigInt @id @default(autoincrement())\n label String\n number Int @default(0)\n data Json?\n}";
    let source = format!("datasource db {{\n provider = \"{provider}\"\n}}\n{source}");
    let (_, schema) = dsl::check(dsl::compile(&source, None).unwrap()).unwrap();
    let target = Target::new(schema.dialect);
    let cases = [
        ("select", r#"{"op":"select","model":"BenchItem","limit":50}"#),
        ("projection", r#"{"op":"select","model":"BenchItem","columns":[{"t":"expr","expr":{"t":"col","path":[],"name":"number"},"name":"n"}],"limit":50}"#),
        ("delete", r#"{"op":"delete","model":"BenchItem","filters":[],"returning":true}"#),
    ];
    let mut results = serde_json::Map::new();
    for (name, json) in cases {
        for _ in 0..100 {
            let op = parse_op(json).unwrap();
            std::hint::black_box(Planner::plan(&schema, target, &op, &NoParams).unwrap());
        }
        #[cfg(feature = "allocation-probe")]
        {
        ALLOCS.store(0, Ordering::Relaxed);
        BYTES.store(0, Ordering::Relaxed);
        for _ in 0..1000 {
            let op = parse_op(json).unwrap();
            std::hint::black_box(Planner::plan(&schema, target, &op, &NoParams).unwrap());
        }
        let allocations = ALLOCS.load(Ordering::Relaxed) / 1000;
        let bytes = BYTES.load(Ordering::Relaxed) / 1000;
        results.insert(name.into(), serde_json::json!({"allocations": allocations, "bytes": bytes}));
        }
        #[cfg(not(feature = "allocation-probe"))]
        {
            let start = std::time::Instant::now();
            for _ in 0..n {
                let op = parse_op(std::hint::black_box(json)).unwrap();
                std::hint::black_box(Planner::plan(&schema,target,&op,&NoParams).unwrap());
            }
            results.insert(name.into(),serde_json::json!({"ns":[start.elapsed().as_nanos() as f64/n as f64]}));
        }
    }
    println!("{}", serde_json::Value::Object(results));
}
