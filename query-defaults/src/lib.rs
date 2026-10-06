//! Optional compiler for model query policies; execution uses prepared host IR.
use orm_contracts::{extension::QueryDefaults, ir::SchemaIr};
use serde_json::{json, Value};
use std::collections::HashMap;

pub fn lower(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations = &ir.behavior.declarations;
    let mut resolved: HashMap<String, QueryDefaults> = ir.behavior.query_defaults.iter().map(|d| (d.model.clone(), d.clone())).collect();
    fn resolve(ir: &SchemaIr, name: &str, resolved: &mut HashMap<String, QueryDefaults>, stack: &mut Vec<String>) -> Result<QueryDefaults, String> {
        if let Some(d) = resolved.get(name) { return Ok(d.clone()); }
        if stack.iter().any(|n| n == name) { return Err(format!("query-default inheritance cycle: {} -> {name}", stack.join(" -> "))); }
        stack.push(name.into());
        let model = ir.models.iter().find(|m| m.name == name).ok_or_else(|| format!("unknown policy parent {name}"))?;
        let ds: Vec<_> = ir.behavior.declarations.iter().filter(|d| d.model == name && d.attribute == "query.defaults").collect();
        if ds.len() > 1 { return Err(format!("duplicate query.defaults on {name}")); }
        let d = ds.first().copied();
        let proxy = ir.behavior.declarations.iter().find(|d| d.model == name && (d.attribute == "proxy.of" || d.attribute == "proxy.model" || d.attribute == "composition.model"));
        // Read additive metadata through its serialized contract so this consumer can
        // compile independently before the proxy contract is merged.
        let metadata = serde_json::to_value(&ir.behavior).map_err(|e| e.to_string())?;
        let proxy_parent = metadata.get("proxy_models").and_then(Value::as_array).and_then(|models| models.iter().find(|m| m["model"] == name)).and_then(|m| m.get("parent"));
        let parent = d.and_then(|d| d.arguments.get("parent")).or(proxy_parent).or_else(|| proxy.and_then(|d| d.arguments.get("parent").or_else(|| d.positional.first()))).and_then(Value::as_str);
        let inherited = match parent { Some(p) => resolve(ir, p, resolved, stack)?, None => QueryDefaults::default() };
        let mut out = inherited.clone(); out.model = name.into(); out.parent = parent.map(str::to_owned);
        if let Some(d) = d {
            if let Some(source) = d.arguments.get("filter") {
                let source = source.as_str().ok_or("filter must be a string")?;
                out.filter = if source == "none" { None } else { Some(Parser::parse(source, inherited.filter.as_ref())?) };
            }
            if let Some(fields) = d.arguments.get("fields") {
                let fields = strings(fields)?;
                out.fields = if fields == ["*"] { None } else { Some(fields) };
            }
            if let Some(related) = d.arguments.get("related") {
                out.related = strings(related)?.into_iter().map(|s| s.split('.').map(str::to_owned).collect()).collect();
            }
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
        let mut left = self.comparison()?;
        loop {
            let (precedence, tag) = match self.peek() { "or" | "||" => (1, "or"), "and" | "&&" => (2, "and"), _ => break };
            if precedence < min { break; }
            self.take()?; let right = self.boolean(precedence + 1)?; left = json!({"t":tag,"items":[left,right]});
        }
        Ok(left)
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
            "!" | "not" => json!({"t":"not","item":self.atom()?}),
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
    pub(super) fn ir() -> SchemaIr {
        serde_json::from_value(json!({"models":[
            {"name":"Parent","table":"p","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"visible","column":"visible","type":"bool"},{"name":"bio","column":"bio","type":"text"}]},
            {"name":"Child","table":"p","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"visible","column":"visible","type":"bool"},{"name":"bio","column":"bio","type":"text"}]}
        ],"behavior":{"declarations":[
            {"model":"Parent","attribute":"query.defaults","field":null,"arguments":{"filter":"visible == true","fields":["id","visible"]},"positional":[],"location":{"file":"test","line":1,"column":1}},
            {"model":"Child","attribute":"query.defaults","field":null,"arguments":{"parent":"Parent","filter":"parent.default_filter and id > 0","fields":["*"]},"positional":[],"location":{"file":"test","line":2,"column":1}},
            {"model":"Child","attribute":"query.selectOut","field":"bio","arguments":{},"positional":[],"location":{"file":"test","line":3,"column":1}}
        ]}})).unwrap()
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
}

#[cfg(test)]
mod reset_tests {
    use super::*;
    #[test]
    fn inheritance_clear_and_replacement_are_distinct() {
        let mut schema = crate::tests::ir();
        schema.behavior.declarations[1].arguments.remove("filter");
        schema.behavior.declarations[1].arguments.remove("fields");
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.query_defaults[0].filter, schema.behavior.query_defaults[1].filter);
        assert_eq!(schema.behavior.query_defaults[0].fields, schema.behavior.query_defaults[1].fields);
        let mut schema = crate::tests::ir();
        schema.behavior.declarations[1].arguments.insert("filter".into(), json!("id == 1"));
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.query_defaults[1].filter.as_ref().unwrap()["t"], "cmp");
        let mut schema = crate::tests::ir();
        schema.behavior.declarations.retain(|d| d.attribute != "query.selectOut");
        schema.behavior.declarations[1].arguments.insert("filter".into(), json!("none"));
        schema.behavior.declarations[1].arguments.insert("related".into(), json!([]));
        lower(&mut schema).unwrap();
        assert!(schema.behavior.query_defaults[1].filter.is_none());
        assert!(schema.behavior.query_defaults[1].fields.is_none());
        assert!(schema.behavior.query_defaults[1].related.is_empty());
    }
}
