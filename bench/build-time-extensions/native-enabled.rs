//! Focused release timings; compile with allocation-probe only to count allocations.
use orm_core::{behavior::NativeModel, dialect::Target, dsl, ir::ValueType};
use orm_engine::{behavior, db::{Cell, DbResult, RowSet}, exec::Outcome, NoParams, parse_op, plan::Planner};
use sea_query::Value;
use std::{hint::black_box, time::Instant};

#[cfg(feature="allocation-probe")]
mod allocations {
    use std::alloc::{GlobalAlloc,Layout,System};
    use std::sync::atomic::{AtomicUsize,Ordering};
    pub struct Count;
    static COUNT:AtomicUsize=AtomicUsize::new(0);
    static BYTES:AtomicUsize=AtomicUsize::new(0);
    unsafe impl GlobalAlloc for Count {
        unsafe fn alloc(&self,layout:Layout)->*mut u8 { COUNT.fetch_add(1,Ordering::Relaxed);BYTES.fetch_add(layout.size(),Ordering::Relaxed);System.alloc(layout) }
        unsafe fn dealloc(&self,ptr:*mut u8,layout:Layout) { System.dealloc(ptr,layout) }
        unsafe fn realloc(&self,ptr:*mut u8,layout:Layout,size:usize)->*mut u8 { COUNT.fetch_add(1,Ordering::Relaxed);BYTES.fetch_add(size,Ordering::Relaxed);System.realloc(ptr,layout,size) }
    }
    pub fn reset() { COUNT.store(0,Ordering::Relaxed);BYTES.store(0,Ordering::Relaxed); }
    pub fn read(n:usize)->serde_json::Value { let count=COUNT.load(Ordering::Relaxed); let bytes=BYTES.load(Ordering::Relaxed); serde_json::json!({"allocations":count as f64/n as f64,"bytes":bytes as f64/n as f64}) }
}
#[cfg(feature="allocation-probe")]
#[global_allocator]
static ALLOCATOR:allocations::Count=allocations::Count;

struct Rows { len:usize,label:String }
impl RowSet for Rows {
    fn len(&self)->usize { self.len }
    fn cell(&self,row:usize,col:usize,_:ValueType)->DbResult<Cell<'_>> {
        Ok(match col { 0=>Cell::BigInt((row+1) as i64),1=>Cell::Text(&self.label),2=>Cell::Int(0),_=>Cell::Null })
    }
    fn value(&self,row:usize,col:usize,_:ValueType)->DbResult<Value> { Ok(match col { 0=>Value::from((row+1) as i64),1=>Value::from(self.label.as_str()),2=>Value::from(0),_=>Value::String(None) }) }
    fn get_i64(&self,row:usize,_:usize)->DbResult<i64> { Ok((row+1) as i64) }
    fn get_bool(&self,_:usize,_:usize)->DbResult<bool> { Ok(false) }
}
fn main() {
    let args:Vec<_>=std::env::args().collect();
    let backend=args.get(1).map(String::as_str).unwrap_or("postgresql");
    let n:usize=args.get(2).map(|v|v.parse().unwrap()).unwrap_or(10000);
    let batches:usize=args.get(3).map(|v|v.parse().unwrap()).unwrap_or(1);
    let source=format!("datasource db {{\n provider = \"{backend}\"\n}}\nmodel BenchItem {{\n id BigInt @id @default(autoincrement())\n label String\n number Int @default(0)\n data Json?\n}}");
    let (_,schema)=dsl::check(dsl::compile(&source,None).unwrap()).unwrap();
    let kind=schema.models[0].native;
    assert_eq!(kind,NativeModel::S0);
    let target=Target::new(schema.dialect);
    let types:Vec<_>=schema.models[0].fields().iter().map(|f|f.value_type()).collect();
    let proxy_source = source.replace("BenchItem", "BenchView");
    let mut proxy_ir = dsl::compile(&proxy_source,None).unwrap();
    proxy_ir.models[0].table = schema.models[0].ir.table.clone();
    let proxy = orm_core::schema::Schema::from_ir(proxy_ir).unwrap();
    let mut owned_json: serde_json::Value = serde_json::from_str(include_str!("ownership-schema.json")).unwrap();
    let mut physical = owned_json["models"].clone();
    physical[1]["fields"].as_array_mut().unwrap().pop();
    owned_json["behavior"]["storage"] = serde_json::json!({"models":physical});
    let mut owned_ir: orm_core::ir::SchemaIr = serde_json::from_value(owned_json).unwrap();
    owned_ir.dialect = schema.dialect;
    let owned = orm_core::schema::Schema::from_ir(owned_ir).unwrap();
    let map=[None,Some(0),None,None,None];
    let fields=[Value::from("alice")];
    let mut results=serde_json::Map::new();
    for name in ["field","record","bulk-validate-50","computed-1","computed-50","computed-1000","plan","dynamic-plan","proxy-plan","composed-plan"] {
        let iterations=if name=="computed-1000" { n/100 } else if name=="computed-50" || name=="bulk-validate-50" { n/10 } else { n };
        let iterations=iterations.max(1);
        let mut index=0;
        let mut operation=|| {
            match name {
                "field"=>{
                    let mut value=Value::from(black_box(" alice "));
                    behavior::field(black_box(kind),black_box(1),&mut value).unwrap();
                    black_box(value);
                }
                "record"=>{ behavior::record(black_box(kind),black_box(&map),black_box(fields.as_slice())).unwrap(); }
                "bulk-validate-50"=>{
                    let mut rows:Vec<_>=(0..50).map(|_|vec![Some(Value::from(black_box(" alice ")))]).collect();
                    behavior::insert_values(black_box(kind),black_box(&map),&mut rows,1).unwrap();
                    black_box(rows);
                }
                name if name.starts_with("computed-")=>{
                    let len=name.split('-').nth(1).unwrap().parse().unwrap();
                    let rows=Box::new(Rows {len,label: black_box("alice").to_owned()});
                    let output=behavior::results(black_box(&[kind]),Outcome::Rows {model:0,rows: black_box(rows),types:types.clone(),shape:None}).unwrap();
                    let Outcome::Rows {rows,types,..}=output else { unreachable!() };
                    black_box(rows.cell(len-1,4,types[4]).unwrap());
                    black_box(rows);
                }
                "proxy-plan" | "composed-plan" => {
                    let (selected,model) = if name == "proxy-plan" { (&proxy,"BenchView") } else { (&owned,"OwnerChild") };
                    let json = format!(r#"{{"op":"select","model":"{model}","limit":50}}"#);
                    let operation = parse_op(black_box(&json)).unwrap();
                    black_box(Planner::plan(selected,target,&operation,&NoParams).unwrap());
                }
                _=>{
                    let json=if name=="dynamic-plan" {
                        index+=1;
                        format!(r#"{{"op":"select","model":"BenchItem","columns":[{{"t":"expr","expr":{{"t":"col","path":[],"name":"display"}},"name":"display_{}"}}],"limit":50}}"#,index%8)
                    } else { r#"{"op":"select","model":"BenchItem","limit":50}"#.to_owned() };
                    let operation=parse_op(black_box(&json)).unwrap();
                    black_box(Planner::plan(&schema,target,&operation,&NoParams).unwrap());
                }
            }
        };
        for _ in 0..1000 { operation(); }
        #[cfg(feature="allocation-probe")]
        {
            allocations::reset();
            for _ in 0..iterations { operation(); }
            let counts = allocations::read(iterations);
            results.insert(name.into(),counts);
        }
        #[cfg(not(feature="allocation-probe"))]
        {
            let mut samples=vec![];
            for _ in 0..batches {
                let start=Instant::now();
                for _ in 0..iterations { operation(); }
                samples.push(start.elapsed().as_nanos() as f64/iterations as f64);
            }
            results.insert(name.into(),serde_json::json!({"ns":samples,"iterations":iterations}));
        }
    }
    println!("{}",serde_json::Value::Object(results));
}
