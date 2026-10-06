//! Durable storage references, independent of ORM and provider SDKs.
use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_SIZE: u64 = (1 << 53) - 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reference {
    v: u8,
    storage: String,
    key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}
impl Reference {
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
    pub fn storage(&self) -> &str {
        &self.storage
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
    pub fn filename(&self) -> Option<&str> {
        self.filename.as_deref()
    }
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }
    pub fn size(&self) -> Option<u64> {
        self.size
    }
}
impl<'de> Deserialize<'de> for Reference {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| D::Error::custom("reference must be an object"))?;
        let allowed = [
            "v",
            "storage",
            "key",
            "version",
            "filename",
            "content_type",
            "size",
        ];
        if object.keys().any(|k| !allowed.contains(&k.as_str()))
            || object.get("v").and_then(|v| v.as_u64()) != Some(1)
        {
            return Err(D::Error::custom("invalid reference version or properties"));
        }
        let text = |name: &str, required: bool| -> Result<Option<String>, D::Error> {
            match object.get(name) {
                None if !required => Ok(None),
                Some(serde_json::Value::String(s))
                    if !s.contains('\0') && (!required || !s.is_empty()) =>
                {
                    Ok(Some(s.clone()))
                }
                _ => Err(D::Error::custom(format!("invalid {name}"))),
            }
        };
        let size = match object.get("size") {
            None => None,
            Some(v) => Some(
                v.as_u64()
                    .filter(|s| *s <= MAX_SIZE)
                    .ok_or_else(|| D::Error::custom("size must be a nonnegative safe integer"))?,
            ),
        };
        Ok(Self {
            v: 1,
            storage: text("storage", true)?.unwrap(),
            key: text("key", true)?.unwrap(),
            version: text("version", false)?,
            filename: text("filename", false)?,
            content_type: text("content_type", false)?,
            size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_vectors_round_trip() {
        let vectors: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../fixtures/references.json")).unwrap();
        for value in vectors {
            let reference = Reference::from_json(&value.to_string()).unwrap();
            assert_eq!(serde_json::to_value(reference).unwrap(), value);
        }
    }
    #[test]
    fn invalid_references_fail() {
        for json in [
            r#"{"v":2,"storage":"a","key":"b"}"#,
            r#"{"v":1,"storage":"","key":"b"}"#,
            r#"{"v":1,"storage":"a","key":"b","size":9007199254740992}"#,
            r#"{"v":1,"storage":"a","key":"b","version":null}"#,
            r#"{"v":1,"storage":"a","key":"b","credentials":"secret"}"#,
        ] {
            assert!(Reference::from_json(json).is_err());
        }
    }
}
