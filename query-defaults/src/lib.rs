//! Optional compiler for model query policies; execution uses prepared host IR.
use orm_contracts::{extension::{OrderKey, QueryDefaults}, ir::{Nulls, SchemaIr}};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// The options of a policy, in `@@query.defaults(<option>: ...)` and `@@query.<option>(...)`.
const OPTIONS: [&str; 5] = ["filter", "fields", "related", "order", "parent"];

/// Each option a model sets, with the declaration that sets it. One option has one source.
fn options(ir: &SchemaIr, name: &str) -> Result<BTreeMap<&'static str, Value>, String> {
    let mut out: BTreeMap<&'static str, (Value, String)> = BTreeMap::new();
    let mut attributes: Vec<&str> = vec![];
    for d in ir.behavior.declarations.iter().filter(|d| d.model == name && d.field.is_none() && d.attribute.starts_with("query.")) {
        let at = format!("{}:{}", d.location.file, d.location.line);
        if attributes.contains(&d.attribute.as_str()) { return Err(format!("{at}: {name}: @@{} is given twice", d.attribute)); }
        attributes.push(&d.attribute);
        let given: Vec<(&'static str, Value, String)> = match d.attribute.trim_start_matches("query.") {
            "defaults" => OPTIONS.iter().filter_map(|o| d.arguments.get(*o).map(|v| (*o, v.clone(), format!("@@query.defaults({o}:) at {at}")))).collect(),
            "order" => vec![("order", Value::Array(d.positional.clone()), format!("@@query.order at {at}"))],
            other => match OPTIONS.iter().find(|o| **o == other) {
                Some(o) => vec![(*o, d.positional.first().cloned().ok_or_else(|| format!("{at}: @@{} needs a value", d.attribute))?, format!("@@{} at {at}", d.attribute))],
                None => continue,
            },
        };
        for (option, value, source) in given {
            if let Some((_, first)) = out.get(option) { return Err(format!("{name}: {option} is set by both {first} and {source}; keep one")); }
            out.insert(option, (value, source));
        }
    }
    Ok(out.into_iter().map(|(k, (v, _))| (k, v)).collect())
}

/// `"-created_at nulls last"`: a field, `-` for descending, and an optional nulls position.
fn order_key(model: &orm_contracts::ir::ModelIr, source: &Value) -> Result<OrderKey, String> {
    let source = source.as_str().ok_or_else(|| format!("{}: an order column is a string such as \"-created_at\"", model.name))?;
    let mut words = source.split_whitespace();
    let first = words.next().ok_or_else(|| format!("{}: empty order column", model.name))?;
    let (desc, field) = match first.strip_prefix('-') { Some(f) => (true, f), None => (false, first) };
    if !model.fields.iter().any(|f| f.name == field) {
        return Err(format!("{}: order column {field:?} is not a field of the model", model.name));
    }
    let nulls = match (words.next(), words.next(), words.next()) {
        (None, None, None) => None,
        (Some("nulls"), Some("first"), None) => Some(Nulls::First),
        (Some("nulls"), Some("last"), None) => Some(Nulls::Last),
        _ => return Err(format!("{}: order column {source:?}: write \"[-]field [nulls first|last]\"", model.name)),
    };
    Ok(OrderKey { field: field.to_owned(), desc, nulls })
}

pub fn lower(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations = &ir.behavior.declarations;
    let mut resolved: HashMap<String, QueryDefaults> = ir.behavior.query_defaults.iter().map(|d| (d.model.clone(), d.clone())).collect();
    fn resolve(ir: &SchemaIr, name: &str, resolved: &mut HashMap<String, QueryDefaults>, stack: &mut Vec<String>) -> Result<QueryDefaults, String> {
        if let Some(d) = resolved.get(name) { return Ok(d.clone()); }
        if stack.iter().any(|n| n == name) { return Err(format!("query-default inheritance cycle: {} -> {name}", stack.join(" -> "))); }
        stack.push(name.into());
        let model = ir.models.iter().find(|m| m.name == name).ok_or_else(|| format!("unknown policy parent {name}"))?;
        let options = options(ir, name)?;
        // The proxy pass (logical phase) fills `proxy_models` before this behavior pass.
        let proxy_parent = ir.behavior.proxy_models.iter().find(|p| p.model == name).map(|p| p.parent.as_str());
        let composed_parent = ir.behavior.declarations.iter().find(|d| d.model == name && d.attribute == "composition.model").and_then(|d| d.arguments.get("parent")).and_then(Value::as_str);
        let parent = options.get("parent").and_then(Value::as_str).or(proxy_parent).or(composed_parent);
        let inherited = match parent { Some(p) => resolve(ir, p, resolved, stack)?, None => QueryDefaults::default() };
        let mut out = inherited.clone(); out.model = name.into(); out.parent = parent.map(str::to_owned);
        if let Some(source) = options.get("filter") {
            let source = source.as_str().ok_or("filter must be a string")?;
            out.filter = if source == "none" { None } else { Some(Parser::parse(source, inherited.filter.as_ref())?) };
        }
        if let Some(fields) = options.get("fields") {
            let fields = strings(fields)?;
            out.fields = if fields == ["*"] { None } else { Some(fields) };
        }
        if let Some(related) = options.get("related") {
            out.related = strings(related)?.into_iter().map(|s| s.split('.').map(str::to_owned).collect()).collect();
        }
        if let Some(order) = options.get("order") {
            out.order = order.as_array().ok_or("order must be a list of strings")?.iter().map(|k| order_key(model, k)).collect::<Result<_, _>>()?;
        }
        let excluded: Vec<&str> = ir.behavior.declarations.iter().filter(|d| d.model == name && d.attribute == "query.selectOut").filter_map(|d| d.field.as_deref()).collect();
        if !excluded.is_empty() {
            let fields = out.fields.get_or_insert_with(|| model.fields.iter().map(|f| f.name.clone()).collect());
            fields.retain(|f| !excluded.contains(&f.as_str()));
        }
        stack.pop(); resolved.insert(name.into(), out.clone()); Ok(out)
    }
    if !declarations.iter().any(|d| d.attribute.starts_with("query.")) && resolved.is_empty() { return Ok(()); }
    for model in &ir.models { resolve(ir, &model.name, &mut resolved, &mut vec![])?; }
    ir.behavior.query_defaults = ir.models.iter().map(|m| resolved.remove(&m.name).expect("resolved model")).collect();
    Ok(())
}
fn strings(value: &Value) -> Result<Vec<String>, String> {
    value.as_array().ok_or("expected a string list")?.iter().map(|v| v.as_str().map(str::to_owned).ok_or("expected a string list".into())).collect()
}

struct Parser<'a> { tokens: Vec<String>, pos: usize, parent: Option<&'a Value> }
impl<'a> Parser<'a> {
    fn parse(source: &str, parent: Option<&'a Value>) -> Result<Value, String> {
        let mut tokens = vec![];
        let mut chars = source.chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_whitespace() { continue; }
            let mut token = c.to_string();
            if c == '"' {
                let mut escaped = false; let mut closed = false;
                for c in chars.by_ref() { token.push(c); if c == '"' && !escaped { closed = true; break; } escaped = c == '\\' && !escaped; }
                if !closed { return Err("unterminated filter string".into()); }
            } else if c.is_alphanumeric() || c == '_' || c == '-' {
                while chars.peek().is_some_and(|c| c.is_alphanumeric() || *c == '_' || *c == '.') { token.push(chars.next().unwrap()); }
            } else if matches!(c, '=' | '!' | '<' | '>' | '&' | '|') && chars.peek().is_some_and(|n| *n == '=' || *n == c) { token.push(chars.next().unwrap()); }
            tokens.push(token);
        }
        let mut p = Self { tokens, pos: 0, parent };
        let out = p.boolean(0)?;
        if p.pos != p.tokens.len() { return Err(format!("unexpected filter token {}", p.tokens[p.pos])); }
        Ok(out)
    }
    fn peek(&self) -> &str { self.tokens.get(self.pos).map(String::as_str).unwrap_or("") }
    fn take(&mut self) -> Result<String, String> { let t = self.tokens.get(self.pos).cloned().ok_or("incomplete default filter")?; self.pos += 1; Ok(t) }
    fn boolean(&mut self, min: u8) -> Result<Value, String> {
        let mut left = self.unary()?;
        loop {
            let (precedence, tag) = match self.peek() { "or" | "||" => (1, "or"), "and" | "&&" => (2, "and"), _ => break };
            if precedence < min { break; }
            self.take()?; let right = self.boolean(precedence + 1)?; left = json!({"t":tag,"items":[left,right]});
        }
        Ok(left)
    }
    /// `not` binds looser than comparisons, as in Python and TypeScript.
    fn unary(&mut self) -> Result<Value, String> {
        if matches!(self.peek(), "!" | "not") { self.take()?; return Ok(json!({"t":"not","item":self.unary()?})); }
        self.comparison()
    }
    fn comparison(&mut self) -> Result<Value, String> {
        let left = self.atom()?;
        let op = match self.peek() { "==" => "eq", "!=" => "ne", "<" => "lt", "<=" => "le", ">" => "gt", ">=" => "ge", _ => return Ok(left) };
        self.take()?; let right = self.atom()?;
        if right.is_null() { if op != "eq" && op != "ne" { return Err("NULL only supports == and !=".into()); } return Ok(json!({"t":"is_null","item":left,"neg":op == "ne"})); }
        if left.is_null() { return Err("put the column before null".into()); }
        Ok(json!({"t":"cmp","op":op,"l":left,"r":right}))
    }
    fn atom(&mut self) -> Result<Value, String> {
        let token = self.take()?;
        Ok(match token.as_str() {
            "(" => { let value = self.boolean(0)?; if self.take()? != ")" { return Err("expected ')'".into()); } value },
            "parent.default_filter" => self.parent.cloned().ok_or("parent.default_filter has no inherited filter")?,
            "null" => Value::Null,
            "true" | "false" => json!({"t":"const","value":token == "true"}),
            _ if token.starts_with('"') => json!({"t":"text","value":serde_json::from_str::<String>(&token).map_err(|e| e.to_string())?}),
            _ if token.parse::<i64>().is_ok() => json!({"t":"int","value":token.parse::<i64>().unwrap()}),
            _ if token.chars().all(|c| c.is_alphanumeric() || c == '_') => json!({"t":"col","path":[],"name":token}),
            _ => return Err(format!("unsupported filter token {token}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ir() -> SchemaIr {
        serde_json::from_value(json!({"models":[
            {"name":"Parent","table":"p","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"visible","column":"visible","type":"bool"},{"name":"bio","column":"bio","type":"text"}]},
            {"name":"Child","table":"p","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"visible","column":"visible","type":"bool"},{"name":"bio","column":"bio","type":"text"}]}
        ],"behavior":{"declarations":[
            {"model":"Parent","attribute":"query.defaults","field":null,"arguments":{"filter":"visible == true","fields":["id","visible"]},"positional":[],"location":{"file":"test","line":1,"column":1}},
            {"model":"Child","attribute":"query.defaults","field":null,"arguments":{"parent":"Parent","filter":"parent.default_filter and id > 0","fields":["*"]},"positional":[],"location":{"file":"test","line":2,"column":1}},
            {"model":"Child","attribute":"query.selectOut","field":"bio","arguments":{},"positional":[],"location":{"file":"test","line":3,"column":1}}
        ]}})).unwrap()
    }
    /// Declared effect `query-defaults`: the prepared policies only.
    #[test]
    fn lowering_changes_only_query_defaults_once() {
        let effects = orm_contracts::extension::pass_effects(&ir(), lower).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.query_defaults"]);
    }
    #[test]
    fn inheritance_replacement_reset_and_select_out() {
        let mut schema = ir(); lower(&mut schema).unwrap();
        let d = &schema.behavior.query_defaults[1];
        assert_eq!(d.fields.as_ref().unwrap(), &["id", "visible"]);
        assert_eq!(d.filter.as_ref().unwrap()["t"], "and");
        let snapshot = serde_json::to_value(&schema).unwrap(); lower(&mut schema).unwrap(); assert_eq!(snapshot, serde_json::to_value(&schema).unwrap());
    }
    #[test]
    fn cycles_and_parent_reference_errors() {
        let mut schema = ir(); schema.behavior.declarations[0].arguments.insert("parent".into(), json!("Child"));
        assert!(lower(&mut schema).unwrap_err().contains("cycle"));
        assert!(Parser::parse("parent.default_filter", None).is_err());
        assert!(Parser::parse("visible == true trailing", None).is_err());
        assert!(Parser::parse("name == \"quoted value\" or id >= 4", None).is_ok());
    }
    #[test]
    fn inheritance_clear_and_replacement_are_distinct() {
        let mut schema = ir();
        schema.behavior.declarations[1].arguments.remove("filter");
        schema.behavior.declarations[1].arguments.remove("fields");
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.query_defaults[0].filter, schema.behavior.query_defaults[1].filter);
        assert_eq!(schema.behavior.query_defaults[0].fields, schema.behavior.query_defaults[1].fields);
        let mut schema = ir();
        schema.behavior.declarations[1].arguments.insert("filter".into(), json!("id == 1"));
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.query_defaults[1].filter.as_ref().unwrap()["t"], "cmp");
        let mut schema = ir();
        schema.behavior.declarations.retain(|d| d.attribute != "query.selectOut");
        schema.behavior.declarations[1].arguments.insert("filter".into(), json!("none"));
        schema.behavior.declarations[1].arguments.insert("related".into(), json!([]));
        lower(&mut schema).unwrap();
        assert!(schema.behavior.query_defaults[1].filter.is_none());
        assert!(schema.behavior.query_defaults[1].fields.is_none());
        assert!(schema.behavior.query_defaults[1].related.is_empty());
    }
    #[test]
    fn proxy_and_composed_models_inherit_through_typed_parents() {
        let mut schema = ir();
        schema.behavior.declarations.retain(|d| d.model != "Child");
        schema.behavior.proxy_models.push(serde_json::from_value(json!({"model":"Child","parent":"Parent"})).unwrap());
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.query_defaults[1].parent.as_deref(), Some("Parent"));
        assert_eq!(schema.behavior.query_defaults[1].filter, schema.behavior.query_defaults[0].filter);
        let mut schema = ir();
        schema.behavior.declarations.retain(|d| d.model != "Child");
        schema.behavior.declarations.push(serde_json::from_value(json!({"model":"Child","attribute":"composition.model","field":null,"arguments":{"parent":"Parent","parentRef":"p","childRef":"c"},"positional":[],"location":{"file":"test","line":4,"column":1}})).unwrap());
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.query_defaults[1].parent.as_deref(), Some("Parent"));
        // An untyped proxy declaration is no parent source; only the lowered contract is.
        let mut schema = ir();
        schema.behavior.declarations.retain(|d| d.model != "Child");
        schema.behavior.declarations.push(serde_json::from_value(json!({"model":"Child","attribute":"proxy.model","field":null,"arguments":{"parent":"Parent"},"positional":[],"location":{"file":"test","line":4,"column":1}})).unwrap());
        lower(&mut schema).unwrap();
        assert!(schema.behavior.query_defaults[1].parent.is_none());
    }
    fn declare(schema: &mut SchemaIr, model: &str, attribute: &str, arguments: Value, positional: Value) {
        let line = schema.behavior.declarations.len() + 1;
        schema.behavior.declarations.push(serde_json::from_value(json!({"model":model,"attribute":attribute,"field":null,"arguments":arguments,"positional":positional,"location":{"file":"test","line":line,"column":1}})).unwrap());
    }
    #[test]
    fn one_attribute_per_option_matches_the_defaults_form() {
        let mut long = ir();
        long.behavior.declarations[0].arguments.insert("related".into(), json!([]));
        long.behavior.declarations[0].arguments.insert("order".into(), json!(["-bio nulls last", "id"]));
        lower(&mut long).unwrap();
        let mut short = ir();
        short.behavior.declarations.retain(|d| d.model != "Parent");
        declare(&mut short, "Parent", "query.filter", json!({}), json!(["visible == true"]));
        declare(&mut short, "Parent", "query.fields", json!({}), json!([["id", "visible"]]));
        declare(&mut short, "Parent", "query.related", json!({}), json!([[]]));
        declare(&mut short, "Parent", "query.order", json!({}), json!(["-bio nulls last", "id"]));
        let child = short.behavior.declarations.iter().position(|d| d.model == "Child").unwrap();
        short.behavior.declarations[child].arguments.remove("parent");
        declare(&mut short, "Child", "query.parent", json!({}), json!(["Parent"]));
        lower(&mut short).unwrap();
        assert_eq!(serde_json::to_value(&long.behavior.query_defaults).unwrap(), serde_json::to_value(&short.behavior.query_defaults).unwrap());
        let order = serde_json::to_value(&short.behavior.query_defaults[1].order).unwrap();
        assert_eq!(order, json!([{"field":"bio","desc":true,"nulls":"last"},{"field":"id"}]), "the child inherits the order");
    }
    #[test]
    fn an_option_has_one_source() {
        let mut schema = ir();
        declare(&mut schema, "Parent", "query.filter", json!({}), json!(["id > 1"]));
        let error = lower(&mut schema).unwrap_err();
        assert!(error.contains("Parent: filter is set by both @@query.defaults(filter:) at test:1 and @@query.filter at test:4"), "{error}");
        let mut schema = ir();
        declare(&mut schema, "Parent", "query.order", json!({}), json!(["id"]));
        declare(&mut schema, "Parent", "query.order", json!({}), json!(["-id"]));
        assert!(lower(&mut schema).unwrap_err().contains("@@query.order is given twice"));
        let mut schema = ir();
        schema.behavior.declarations[0].arguments.insert("order".into(), json!(["id"]));
        declare(&mut schema, "Parent", "query.order", json!({}), json!(["-id"]));
        assert!(lower(&mut schema).unwrap_err().contains("order is set by both @@query.defaults(order:)"));
    }
    #[test]
    fn default_order_columns_and_clearing() {
        let order = |child: Value| {
            let mut schema = ir();
            schema.behavior.declarations[0].arguments.insert("order".into(), json!(["-id"]));
            declare(&mut schema, "Child", "query.order", json!({}), child);
            lower(&mut schema).map(|_| serde_json::to_value(&schema.behavior.query_defaults[1].order).unwrap())
        };
        assert_eq!(order(json!([])).unwrap(), json!([]), "@@query.order() clears the inherited order");
        assert_eq!(order(json!(["visible nulls first"])).unwrap(), json!([{"field":"visible","nulls":"first"}]));
        assert!(order(json!(["nope"])).unwrap_err().contains("\"nope\" is not a field of the model"));
        assert!(order(json!(["id desc"])).unwrap_err().contains("write \"[-]field [nulls first|last]\""));
        assert!(order(json!(["-id nulls"])).is_err());
        let mut schema = ir();
        schema.behavior.declarations[1].arguments.insert("order".into(), json!([]));
        schema.behavior.declarations[0].arguments.insert("order".into(), json!(["id"]));
        lower(&mut schema).unwrap();
        assert!(schema.behavior.query_defaults[1].order.is_empty(), "order: [] clears it too");
        assert_eq!(schema.behavior.query_defaults[0].order.len(), 1);
    }
    #[test]
    fn a_proxy_inherits_the_default_order() {
        let mut schema = ir();
        schema.behavior.declarations.retain(|d| d.model != "Child");
        declare(&mut schema, "Parent", "query.order", json!({}), json!(["-visible", "id"]));
        schema.behavior.proxy_models.push(serde_json::from_value(json!({"model":"Child","parent":"Parent"})).unwrap());
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.query_defaults[1].order, schema.behavior.query_defaults[0].order);
        assert_eq!(schema.behavior.query_defaults[1].order.len(), 2);
    }
    #[test]
    fn operator_precedence_and_null() {
        let not = Parser::parse("not id == 1", None).unwrap();
        assert_eq!((not["t"].as_str(), not["item"]["t"].as_str()), (Some("not"), Some("cmp")));
        assert_eq!(Parser::parse("! visible", None).unwrap()["item"]["t"], "col");
        assert_eq!(Parser::parse("a or b and c", None).unwrap()["items"][1]["t"], "and");
        assert_eq!(Parser::parse("a and b or c", None).unwrap()["items"][0]["t"], "and");
        assert_eq!(Parser::parse("a != null", None).unwrap()["neg"], true);
        assert_eq!(Parser::parse("a == null", None).unwrap()["neg"], false);
        assert!(Parser::parse("a < null", None).is_err());
    }
}
