//! Error types for the ontology engine.
//!
//! All fallible operations return [`OntologyError`]. Validation routines that
//! can produce more than one problem (e.g. validating every property on an
//! instance) return [`OntologyError::SchemaValidation`], which aggregates the
//! individual [`ValidationIssue`]s so callers get the full picture in one
//! round trip instead of fixing errors one at a time.

use thiserror::Error;

/// A single, specific validation problem found while checking an
/// [`crate::types::ObjectInstance`] against its [`crate::types::ObjectType`] schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationIssue {
    /// A property required by the schema was not supplied.
    MissingProperty { property: String },
    /// A property was supplied whose value's runtime type does not match
    /// the type declared in the schema.
    TypeMismatch {
        property: String,
        expected: String,
        found: String,
    },
    /// A property was supplied that is not declared anywhere in the schema.
    UnknownProperty { property: String },
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationIssue::MissingProperty { property } => {
                write!(f, "missing required property '{property}'")
            }
            ValidationIssue::TypeMismatch {
                property,
                expected,
                found,
            } => write!(
                f,
                "property '{property}' expected type {expected}, found {found}"
            ),
            ValidationIssue::UnknownProperty { property } => {
                write!(f, "property '{property}' is not declared in the schema")
            }
        }
    }
}

/// Top level error type for every fallible [`crate::engine::OntologyEngine`] operation.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OntologyError {
    #[error("object type '{0}' is not registered")]
    ObjectTypeNotRegistered(String),

    #[error("link type '{0}' is not registered")]
    LinkTypeNotRegistered(String),

    #[error("object type '{0}' is already registered")]
    ObjectTypeAlreadyRegistered(String),

    #[error("link type '{0}' is already registered")]
    LinkTypeAlreadyRegistered(String),

    #[error("object type '{name}' has an empty primary key name")]
    EmptyPrimaryKey { name: String },

    #[error("object instance id '{0}' already exists")]
    DuplicateInstance(String),

    #[error("object instance id '{0}' was not found")]
    InstanceNotFound(String),

    #[error("link type '{link_type}' requires source type '{expected_source}' but instance '{source_id}' has type '{actual_source}'")]
    LinkSourceTypeMismatch {
        link_type: String,
        expected_source: String,
        actual_source: String,
        source_id: String,
    },

    #[error("link type '{link_type}' requires target type '{expected_target}' but instance '{target_id}' has type '{actual_target}'")]
    LinkTargetTypeMismatch {
        link_type: String,
        expected_target: String,
        actual_target: String,
        target_id: String,
    },

    #[error("link already exists: {link_type} {source_id} -> {target_id}")]
    DuplicateLink {
        link_type: String,
        source_id: String,
        target_id: String,
    },

    #[error("schema validation failed for instance '{instance_id}' ({issue_count} issue(s)): {}", format_issues(.issues))]
    SchemaValidation {
        instance_id: String,
        issue_count: usize,
        issues: Vec<ValidationIssue>,
    },

    #[error("cannot delete object type '{0}': instances of this type still exist")]
    ObjectTypeInUse(String),

    #[error("primary key property '{property}' cannot be modified on instance '{instance_id}'")]
    ImmutablePrimaryKey {
        instance_id: String,
        property: String,
    },
}

fn format_issues(issues: &[ValidationIssue]) -> String {
    issues
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

pub type Result<T> = std::result::Result<T, OntologyError>;
