//! Optional native generic routing. Names are resolved once at schema preparation.
pub use orm_contracts::generic::*;
use crate::{ir::{ColType, FieldIr}, schema::Schema};
use orm_contracts::{extension::{FieldId, ModelId}, identity::IdentityManifest};

fn compatible(a: &FieldIr, b: &FieldIr) -> bool {
    a.ty == b.ty && !a.array && !b.array && a.enum_name.is_none() && b.enum_name.is_none()
        && a.db_type == b.db_type && a.read_sql == b.read_sql && a.write_sql == b.write_sql && a.max_length == b.max_length
}

pub fn resolve(schema: &Schema, identities: &IdentityManifest, declarations: &[GenericRelation], reverse: &[GenericReverse]) -> Result<(Vec<PreparedGenericRelation>, Vec<PreparedGenericReverse>), String> {
    identities.validate()?;
    let mut prepared = vec![];
    for declaration in declarations {
        let model = ModelId(schema.model_idx(&declaration.model)?);
        let source = schema.model(model.0);
        if source.fields().iter().any(|f| f.name == declaration.name) || source.ir.relations.iter().any(|r| r.name == declaration.name)
            || prepared.iter().any(|r: &PreparedGenericRelation| r.model == model && r.name == declaration.name) {
            return Err(format!("{}.{}: generic member collision", declaration.model, declaration.name));
        }
        let discriminator = FieldId { model, position: source.field_pos(&declaration.type_field)? };
        let key = FieldId { model, position: source.field_pos(&declaration.key_field)? };
        let ty = &source.fields()[discriminator.position];
        let key_field = &source.fields()[key.position];
        if discriminator == key || ty.ty != ColType::Int || ty.enum_name.as_deref() != Some("ContentType") || ty.array || ty.db_type.is_some() || ty.read_sql.is_some() || ty.write_sql.is_some() || ty.primary_key || key_field.primary_key || ty.nullable != key_field.nullable {
            return Err(format!("{}.{}: invalid ContentType/key storage pair", declaration.model, declaration.name));
        }
        if prepared.iter().any(|r| r.model == model && [r.discriminator, r.key].iter().any(|f| *f == discriminator || *f == key)) {
            return Err("generic relations must not share pair fields".into());
        }
        let mut targets = vec![];
        for target in &declaration.targets {
            let identity = identities.active().find(|e| &e.model == target).ok_or_else(|| format!("{target} is not an active concrete ContentType"))?;
            let target_model = ModelId(schema.model_idx(target)?);
            let pk = schema.model(target_model.0).pk_field();
            if !compatible(key_field, pk) || !matches!(pk.ty, ColType::Int | ColType::BigInt | ColType::Uuid | ColType::String | ColType::Text) {
                return Err(format!("{target} has an incompatible generic key codec"));
            }
            if targets.iter().any(|t: &GenericTarget| t.content_type == identity.id) { return Err("duplicate generic target identity".into()); }
            targets.push(GenericTarget { content_type: identity.id, model: target_model, key: FieldId { model: target_model, position: schema.model(target_model.0).pk } });
        }
        if targets.is_empty() { return Err("generic relation needs at least one concrete target".into()); }
        targets.sort_by_key(|t| t.content_type);
        prepared.push(PreparedGenericRelation { model, name: declaration.name.clone(), discriminator, key, nullable: ty.nullable, targets });
    }
    let mut reverses = vec![];
    for declaration in reverse {
        let model = ModelId(schema.model_idx(&declaration.model)?);
        let source = ModelId(schema.model_idx(&declaration.source)?);
        let relation = prepared.iter().position(|r| r.model == source && r.name == declaration.relation).ok_or("reverse names an unknown generic relation")?;
        let content_type = prepared[relation].targets.iter().find(|t| t.model == model).ok_or("reverse owner is not an allowed concrete target")?.content_type;
        let owner = schema.model(model.0);
        if owner.fields().iter().any(|f| f.name == declaration.name) || owner.ir.relations.iter().any(|r| r.name == declaration.name)
            || prepared.iter().any(|r| r.model == model && r.name == declaration.name)
            || reverses.iter().any(|r: &PreparedGenericReverse| r.model == model && r.name == declaration.name) {
            return Err("generic reverse member collision".into());
        }
        reverses.push(PreparedGenericReverse { model, name: declaration.name.clone(), source, relation, content_type });
    }
    Ok((prepared, reverses))
}

pub trait Routing {
    fn route(&self, content_type: i32) -> Result<&GenericTarget, String>;
    fn target(&self, model: ModelId) -> Result<&GenericTarget, String>;
}
impl Routing for PreparedGenericRelation {
    fn route(&self, content_type: i32) -> Result<&GenericTarget, String> {
        let index = self.targets.binary_search_by_key(&content_type, |t| t.content_type).map_err(|_| format!("unknown or disallowed ContentType {content_type} for {}", self.name))?;
        Ok(&self.targets[index])
    }
    fn target(&self, model: ModelId) -> Result<&GenericTarget, String> {
        self.targets.iter().find(|t| t.model == model).ok_or_else(|| format!("target model is not allowed by {}", self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_resolves_overlapping_keys_and_fixed_reverse_discriminator() {
        let mut ir: crate::ir::SchemaIr = serde_json::from_value(serde_json::json!({
            "models":[
                {"name":"Post","table":"posts","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]},
                {"name":"Photo","table":"photos","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]},
                {"name":"Tag","table":"tags","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"kind","column":"kind","type":"int","enum":"ContentType"},{"name":"object_id","column":"object_id","type":"int"}]}
            ]
        })).unwrap();
        let identities = crate::identity::reconcile(&Default::default(), &["Post".into(), "Photo".into(), "Tag".into()], &[], &[]).unwrap();
        ir.enums = vec![crate::identity::content_type(&identities).unwrap()];
        ir.identities = Some(identities.clone());
        let schema = Schema::from_ir(ir).unwrap();
        let forward = GenericRelation { model:"Tag".into(), name:"target".into(), type_field:"kind".into(), key_field:"object_id".into(), targets:vec!["Post".into(),"Photo".into()] };
        let reverse = GenericReverse { model:"Post".into(), name:"tags".into(), source:"Tag".into(), relation:"target".into() };
        let (routes, reverses) = resolve(&schema, &identities, &[forward], &[reverse]).unwrap();
        assert_eq!(routes[0].route(1).unwrap().model, ModelId(1));
        assert_eq!(routes[0].route(2).unwrap().model, ModelId(0));
        assert!(routes[0].route(3).is_err());
        assert!(routes[0].route(0).is_err());
        assert!(routes[0].target(ModelId(2)).is_err());
        assert_eq!(routes[0].target(ModelId(0)).unwrap().content_type, 2);
        assert_eq!(reverses[0].content_type, 2);
        assert_eq!(routes[0].discriminator.position, 1);
        assert_eq!(routes[0].key.position, 2);
    }
}
