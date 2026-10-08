//! `@client_default`: values the ORM fills into omitted insert values. Definition
//! parses each literal once; inserts copy it or make a new value for each row.
use crate::ir::{ClientCall, ClientDefaultIr, ColType, EnumIr, FieldIr, ValueType};
use serde_json::Value;

#[derive(Clone, Debug)]
pub enum ClientDefault {
    Literal(Literal),
    Call(ClientCall),
}

#[derive(Clone, Debug)]
pub enum Literal {
    Null(ValueType), Int(i32), BigInt(i64), Float(f64), Bool(bool), Text(String),
    DateTime(chrono::DateTime<chrono::FixedOffset>), Date(chrono::NaiveDate),
    Uuid(uuid::Uuid), Decimal(bigdecimal::BigDecimal), Json(Value), Array(ColType, Vec<Literal>),
}

impl Literal {
    fn parse(value: &Value, ty: ValueType) -> Result<Self, String> {
        // On a Json field, null is the JSON value, like `@default("null")`.
        if ty.ty == ColType::Json && !ty.array { return Ok(Self::Json(value.clone())); }
        if value.is_null() { return Ok(Self::Null(ty)); }
        if ty.array {
            return Ok(Self::Array(ty.ty, value.as_array().ok_or("requires an array")?.iter().map(|v| Self::parse(v, ty.element())).collect::<Result<_, _>>()?));
        }
        let text = || value.as_str().ok_or("requires text");
        Ok(match ty.ty {
            ColType::Int => Self::Int(value.as_i64().and_then(|v| i32::try_from(v).ok()).ok_or("requires int32")?),
            ColType::BigInt => Self::BigInt(value.as_i64().ok_or("requires int64")?),
            ColType::Float => Self::Float(value.as_f64().ok_or("requires a number")?),
            ColType::Bool => Self::Bool(value.as_bool().ok_or("requires a bool")?),
            ColType::String | ColType::Text => Self::Text(text()?.into()),
            ColType::DateTime => Self::DateTime(chrono::DateTime::parse_from_rfc3339(text()?)
                .map_err(|e| format!("requires an RFC 3339 timestamp with an offset, such as 2026-01-01T00:00:00Z: {e}"))?),
            ColType::Date => Self::Date(chrono::NaiveDate::parse_from_str(text()?, "%Y-%m-%d").map_err(|e| e.to_string())?),
            ColType::Uuid => Self::Uuid(text()?.parse::<uuid::Uuid>().map_err(|e| e.to_string())?),
            ColType::Decimal => {
                let text = match value { Value::Number(n) => n.to_string(), _ => text()?.to_owned() };
                Self::Decimal(text.parse::<bigdecimal::BigDecimal>().map_err(|e| e.to_string())?)
            }
            ColType::Json => Self::Json(value.clone()),
        })
    }
}

/// The client defaults of a model's fields, by field position.
pub fn prepare(model: &str, fields: &[FieldIr], enums: &[EnumIr]) -> Result<Vec<(usize, ClientDefault)>, String> {
    let mut out = vec![];
    for (position, f) in fields.iter().enumerate() {
        let Some(default) = &f.client_default else { continue };
        let what = |e: &str| format!("{model}.{}: @client_default {e}", f.name);
        let prepared = match default {
            ClientDefaultIr::Value(value) => {
                if let Some(e) = f.enum_idx.and_then(|i| enums.get(i as usize)) {
                    let values: Vec<&Value> = match value.as_array() { Some(a) if f.array => a.iter().collect(), _ => vec![value] };
                    if values.iter().any(|v| !v.is_null() && !e.values.iter().any(|m| m.value == **v)) {
                        return Err(what(&format!("must be a value of enum {}", e.name)));
                    }
                }
                ClientDefault::Literal(Literal::parse(value, f.value_type()).map_err(|e| what(&e))?)
            }
            ClientDefaultIr::Call(call) => {
                let fits = !f.array && match call {
                    ClientCall::Uuid | ClientCall::Uuid7 => f.enum_name.is_none() && matches!(f.ty, ColType::Uuid | ColType::String | ColType::Text),
                    ClientCall::Now => matches!(f.ty, ColType::DateTime | ColType::Date),
                };
                if !fits { return Err(what(&format!("{call:?} does not fit a {:?} field", f.ty).to_lowercase())); }
                ClientDefault::Call(*call)
            }
        };
        out.push((position, prepared));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(name: &str, ty: ColType, default: ClientDefaultIr) -> FieldIr {
        FieldIr { client_default: Some(default), ..FieldIr::plain(name, ty) }
    }

    #[test]
    fn literals_parse_at_definition_and_null_is_kept() {
        let fields = [field("date", ColType::Date, ClientDefaultIr::Value(json!("2026-10-05"))),
            field("name", ColType::String, ClientDefaultIr::Value(json!(null))),
            field("id", ColType::Uuid, ClientDefaultIr::Call(ClientCall::Uuid7)),
            FieldIr::plain("plain", ColType::Int)];
        let prepared = prepare("M", &fields, &[]).unwrap();
        assert!(matches!(prepared[0], (0, ClientDefault::Literal(Literal::Date(_)))));
        assert!(matches!(prepared[1], (1, ClientDefault::Literal(Literal::Null(_)))));
        assert!(matches!(prepared[2], (2, ClientDefault::Call(ClientCall::Uuid7))));
        assert_eq!(prepared.len(), 3);
        let bad = [field("date", ColType::Date, ClientDefaultIr::Value(json!("bad date")))];
        assert!(prepare("M", &bad, &[]).unwrap_err().contains("M.date: @client_default"));
    }

    #[test]
    fn json_null_decimal_numbers_and_datetime_offsets() {
        let fields = [field("a", ColType::Json, ClientDefaultIr::Value(json!(null))),
            field("d", ColType::Decimal, ClientDefaultIr::Value(json!(1.5)))];
        let prepared = prepare("M", &fields, &[]).unwrap();
        assert!(matches!(&prepared[0].1, ClientDefault::Literal(Literal::Json(Value::Null))));
        assert!(matches!(&prepared[1].1, ClientDefault::Literal(Literal::Decimal(d)) if d.to_string() == "1.5"));
        let error = prepare("M", &[field("t", ColType::DateTime, ClientDefaultIr::Value(json!("2026-01-01T00:00:00")))], &[]).unwrap_err();
        assert!(error.contains("requires an RFC 3339 timestamp with an offset"), "{error}");
    }

    #[test]
    fn calls_fit_only_their_types() {
        for (ty, call) in [(ColType::Int, ClientCall::Uuid), (ColType::Text, ClientCall::Now), (ColType::Bool, ClientCall::Uuid7)] {
            assert!(prepare("M", &[field("f", ty, ClientDefaultIr::Call(call))], &[]).unwrap_err().contains("does not fit"));
        }
        for (ty, call) in [(ColType::String, ClientCall::Uuid), (ColType::Date, ClientCall::Now), (ColType::DateTime, ClientCall::Now)] {
            assert!(prepare("M", &[field("f", ty, ClientDefaultIr::Call(call))], &[]).is_ok());
        }
        let array = FieldIr { array: true, ..field("f", ColType::String, ClientDefaultIr::Call(ClientCall::Uuid)) };
        assert!(prepare("M", &[array], &[]).unwrap_err().contains("does not fit"));
        let status: EnumIr = serde_json::from_value(json!({"name": "Status", "db_name": "status", "storage": "text", "values": [{"name": "A", "value": "a"}]})).unwrap();
        let on_enum = |default| FieldIr { enum_name: Some("Status".into()), enum_idx: Some(0), ..field("f", ColType::String, default) };
        assert!(prepare("M", &[on_enum(ClientDefaultIr::Call(ClientCall::Uuid))], std::slice::from_ref(&status)).unwrap_err().contains("does not fit"));
        assert!(prepare("M", &[on_enum(ClientDefaultIr::Value(json!("zzz")))], std::slice::from_ref(&status)).unwrap_err().contains("must be a value of enum Status"));
        assert!(prepare("M", &[on_enum(ClientDefaultIr::Value(json!("a")))], std::slice::from_ref(&status)).is_ok());
    }
}
