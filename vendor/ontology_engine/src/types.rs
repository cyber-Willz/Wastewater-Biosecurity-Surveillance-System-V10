//! Core domain types: schema definitions (`ObjectType`, `LinkType`) and
//! instance data (`ObjectInstance`, `LinkInstance`), plus the runtime
//! property value representation `PropertyValue`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Runtime value stored on an [`ObjectInstance`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum PropertyValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
}

impl PropertyValue {
    /// The [`PropertyType`] this value would satisfy.
    pub fn type_of(&self) -> PropertyType {
        match self {
            PropertyValue::String(_) => PropertyType::String,
            PropertyValue::Integer(_) => PropertyType::Integer,
            PropertyValue::Float(_) => PropertyType::Float,
            PropertyValue::Boolean(_) => PropertyType::Boolean,
        }
    }
}

impl fmt::Display for PropertyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PropertyValue::String(s) => write!(f, "{s}"),
            PropertyValue::Integer(i) => write!(f, "{i}"),
            PropertyValue::Float(v) => write!(f, "{v}"),
            PropertyValue::Boolean(b) => write!(f, "{b}"),
        }
    }
}

/// The declared type of a schema property. Distinct from [`PropertyValue`]
/// so schemas can be defined without constructing dummy values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropertyType {
    String,
    Integer,
    Float,
    Boolean,
}

impl fmt::Display for PropertyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PropertyType::String => "String",
            PropertyType::Integer => "Integer",
            PropertyType::Float => "Float",
            PropertyType::Boolean => "Boolean",
        };
        write!(f, "{s}")
    }
}

/// Schema definition for a class of objects (e.g. "Employee", "Aircraft").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectType {
    pub name: String,
    pub primary_key: String,
    /// Declared property name -> declared type. All declared properties are
    /// required and no undeclared properties are accepted on instances of
    /// this type.
    pub property_schemas: HashMap<String, PropertyType>,
}

/// Builder for [`ObjectType`], since schemas are typically assembled once at
/// startup from several `.property(...)` calls.
pub struct ObjectTypeBuilder {
    name: String,
    primary_key: Option<String>,
    property_schemas: HashMap<String, PropertyType>,
}

impl ObjectTypeBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            primary_key: None,
            property_schemas: HashMap::new(),
        }
    }

    pub fn primary_key(mut self, key: impl Into<String>) -> Self {
        self.primary_key = Some(key.into());
        self
    }

    /// Declare a property. If this is the primary key it must also match
    /// the type used in instance data (typically `String` or `Integer`).
    pub fn property(mut self, name: impl Into<String>, ty: PropertyType) -> Self {
        self.property_schemas.insert(name.into(), ty);
        self
    }

    /// Finalize the builder. Returns an error string if the primary key was
    /// never set or was never declared as a property.
    pub fn build(self) -> Result<ObjectType, String> {
        let primary_key = self
            .primary_key
            .ok_or_else(|| "primary_key must be set".to_string())?;
        if primary_key.is_empty() {
            return Err("primary_key must not be empty".to_string());
        }
        if !self.property_schemas.contains_key(&primary_key) {
            return Err(format!(
                "primary key '{primary_key}' must also be declared via .property(...)"
            ));
        }
        Ok(ObjectType {
            name: self.name,
            primary_key,
            property_schemas: self.property_schemas,
        })
    }
}

/// Schema definition for a directed relationship between two object types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkType {
    pub name: String,
    pub source_type: String,
    pub target_type: String,
}

impl LinkType {
    pub fn new(
        name: impl Into<String>,
        source_type: impl Into<String>,
        target_type: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            source_type: source_type.into(),
            target_type: target_type.into(),
        }
    }
}

/// A live instance of an [`ObjectType`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectInstance {
    pub id: String,
    pub object_type: String,
    pub properties: HashMap<String, PropertyValue>,
}

impl ObjectInstance {
    pub fn new(
        id: impl Into<String>,
        object_type: impl Into<String>,
        properties: HashMap<String, PropertyValue>,
    ) -> Self {
        Self {
            id: id.into(),
            object_type: object_type.into(),
            properties,
        }
    }
}

/// A live, directed instance of a [`LinkType`] connecting two
/// [`ObjectInstance`]s by id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LinkInstance {
    pub link_type: String,
    pub source_id: String,
    pub target_id: String,
}

impl LinkInstance {
    pub fn new(
        link_type: impl Into<String>,
        source_id: impl Into<String>,
        target_id: impl Into<String>,
    ) -> Self {
        Self {
            link_type: link_type.into(),
            source_id: source_id.into(),
            target_id: target_id.into(),
        }
    }
}

/// Direction of traversal from a given instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Follow links where the given instance is the source.
    Outgoing,
    /// Follow links where the given instance is the target.
    Incoming,
}
