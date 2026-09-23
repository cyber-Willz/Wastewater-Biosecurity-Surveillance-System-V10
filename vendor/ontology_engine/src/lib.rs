//! `ontology_engine`: a small, dependency-light, thread-safe in-memory
//! ontology / knowledge-graph engine in the spirit of Palantir Foundry's
//! Object/Link model.
//!
//! ```
//! use ontology_engine::prelude::*;
//! use std::collections::HashMap;
//!
//! let engine = OntologyEngine::new();
//!
//! let employee = ObjectTypeBuilder::new("Employee")
//!     .primary_key("emp_id")
//!     .property("emp_id", PropertyType::Integer)
//!     .property("name", PropertyType::String)
//!     .build()
//!     .unwrap();
//! engine.register_object_type(employee).unwrap();
//!
//! engine
//!     .create_object_instance(ObjectInstance::new(
//!         "emp_101",
//!         "Employee",
//!         HashMap::from([
//!             ("emp_id".to_string(), PropertyValue::Integer(101)),
//!             ("name".to_string(), PropertyValue::String("Maverick".into())),
//!         ]),
//!     ))
//!     .unwrap();
//!
//! assert_eq!(engine.instance_count(), 1);
//! ```

pub mod engine;
pub mod error;
pub mod types;

pub mod prelude {
    pub use crate::engine::OntologyEngine;
    pub use crate::error::{OntologyError, Result, ValidationIssue};
    pub use crate::types::{
        Direction, LinkInstance, LinkType, ObjectInstance, ObjectType, ObjectTypeBuilder,
        PropertyType, PropertyValue,
    };
}
