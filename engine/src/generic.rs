//! Optional borrowed native pair validation; no database existence lookup.
use orm_core::{generic::{PreparedGenericRelation, GenericTarget, Routing}, ir::{ColType, ValueType}};
use sea_query::Value;
use crate::{Error, Result, params::null_of};

/// Omitted fields differ from explicit NULL. Partial updates rely on the ordinary
/// pair CHECK constraint to validate the resulting stored row atomically.
pub fn validate_pair<'a>(relation: &'a PreparedGenericRelation, key_type: ValueType, discriminator: Option<&Value>, key: Option<&Value>, insert: bool) -> Result<Option<&'a GenericTarget>> {
    if discriminator.is_none() && key.is_none() {
        return if insert && !relation.nullable { Err(Error::query("required generic pair must be supplied")) } else { Ok(None) };
    }
    if insert && (discriminator.is_none() || key.is_none()) { return Err(Error::query("generic insert requires both type and key fields")); }
    let key_null = key.is_some_and(|v| v == &null_of(Some(key_type)));
    let type_null = matches!(discriminator, Some(Value::Int(None)));
    if discriminator.is_some() && key.is_some() && type_null != key_null { return Err(Error::query("generic pair must be fully null or fully present")); }
    if (type_null || key_null) && !relation.nullable { return Err(Error::query("required generic pair cannot be cleared")); }
    if let Some(key) = key.filter(|_| !key_null) {
        let valid = matches!((key_type.ty, key),
            (ColType::Int, Value::Int(Some(_))) | (ColType::BigInt, Value::BigInt(Some(_))) |
            (ColType::Uuid, Value::Uuid(Some(_))) | (ColType::String | ColType::Text, Value::String(Some(_))));
        if !valid { return Err(Error::query("generic object ID has incompatible key representation")); }
    }
    match discriminator {
        Some(Value::Int(Some(value))) => relation.route(*value).map(Some).map_err(Error::query),
        Some(Value::Int(None)) | None => Ok(None),
        _ => Err(Error::query("generic discriminator must be a ContentType integer")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orm_core::behavior::{ModelId, FieldId};
    fn relation(nullable: bool) -> PreparedGenericRelation {
        let model = ModelId(2);
        PreparedGenericRelation { model, name:"target".into(), discriminator:FieldId {model,position:1}, key:FieldId {model,position:2}, nullable,
            targets:vec![GenericTarget{content_type:1,model:ModelId(0),key:FieldId{model:ModelId(0),position:0}},GenericTarget{content_type:2,model:ModelId(1),key:FieldId{model:ModelId(1),position:0}}] }
    }
    #[test]
    fn pair_routes_same_keys_to_distinct_targets_and_rejects_half_pairs() {
        let r = relation(true); let ty = ValueType::scalar(ColType::Int); let key = Value::Int(Some(7));
        assert_eq!(validate_pair(&r,ty,Some(&Value::Int(Some(1))),Some(&key),true).unwrap().unwrap().model,ModelId(0));
        assert_eq!(validate_pair(&r,ty,Some(&Value::Int(Some(2))),Some(&key),true).unwrap().unwrap().model,ModelId(1));
        assert!(validate_pair(&r,ty,Some(&Value::Int(Some(3))),Some(&key),true).is_err());
        assert!(validate_pair(&r,ty,Some(&Value::Int(None)),Some(&key),true).is_err());
        assert!(validate_pair(&r,ty,None,Some(&key),true).is_err());
        assert!(validate_pair(&r,ty,Some(&Value::Int(Some(1))),Some(&Value::BigInt(Some(7))),true).is_err());
        assert!(validate_pair(&r,ty,Some(&Value::Int(None)),Some(&Value::Int(None)),true).unwrap().is_none());
    }
    #[test]
    fn required_omissions_and_clear_fail_but_partial_update_is_checked_by_storage() {
        let r = relation(false); let ty = ValueType::scalar(ColType::Int);
        assert!(validate_pair(&r,ty,None,None,true).is_err());
        assert!(validate_pair(&r,ty,Some(&Value::Int(None)),Some(&Value::Int(None)),false).is_err());
        assert!(validate_pair(&r,ty,None,None,false).is_ok());
        assert!(validate_pair(&r,ty,Some(&Value::Int(Some(2))),None,false).is_ok());
        assert!(validate_pair(&r,ty,Some(&Value::Int(Some(9))),None,false).is_err());
    }
}
