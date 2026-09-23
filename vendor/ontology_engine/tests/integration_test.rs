use ontology_engine::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

fn employee_type() -> ObjectType {
    ObjectTypeBuilder::new("Employee")
        .primary_key("emp_id")
        .property("emp_id", PropertyType::Integer)
        .property("name", PropertyType::String)
        .build()
        .unwrap()
}

fn aircraft_type() -> ObjectType {
    ObjectTypeBuilder::new("Aircraft")
        .primary_key("tail_number")
        .property("tail_number", PropertyType::String)
        .property("model", PropertyType::String)
        .build()
        .unwrap()
}

fn engine_with_schema() -> OntologyEngine {
    let engine = OntologyEngine::new();
    engine.register_object_type(employee_type()).unwrap();
    engine.register_object_type(aircraft_type()).unwrap();
    engine
        .register_link_type(LinkType::new("flies", "Employee", "Aircraft"))
        .unwrap();
    engine
}

fn pilot(id: &str, emp_id: i64, name: &str) -> ObjectInstance {
    ObjectInstance::new(
        id,
        "Employee",
        HashMap::from([
            ("emp_id".to_string(), PropertyValue::Integer(emp_id)),
            ("name".to_string(), PropertyValue::String(name.to_string())),
        ]),
    )
}

fn aircraft(id: &str, tail: &str, model: &str) -> ObjectInstance {
    ObjectInstance::new(
        id,
        "Aircraft",
        HashMap::from([
            (
                "tail_number".to_string(),
                PropertyValue::String(tail.to_string()),
            ),
            (
                "model".to_string(),
                PropertyValue::String(model.to_string()),
            ),
        ]),
    )
}

#[test]
fn register_duplicate_object_type_fails() {
    let engine = OntologyEngine::new();
    engine.register_object_type(employee_type()).unwrap();
    let err = engine.register_object_type(employee_type()).unwrap_err();
    assert_eq!(err, OntologyError::ObjectTypeAlreadyRegistered("Employee".into()));
}

#[test]
fn register_link_type_requires_known_object_types() {
    let engine = OntologyEngine::new();
    engine.register_object_type(employee_type()).unwrap();
    let err = engine
        .register_link_type(LinkType::new("flies", "Employee", "Aircraft"))
        .unwrap_err();
    assert_eq!(err, OntologyError::ObjectTypeNotRegistered("Aircraft".into()));
}

#[test]
fn create_instance_missing_object_type_fails() {
    let engine = OntologyEngine::new();
    let err = engine.create_object_instance(pilot("emp_1", 1, "X")).unwrap_err();
    assert_eq!(err, OntologyError::ObjectTypeNotRegistered("Employee".into()));
}

#[test]
fn create_instance_missing_required_property_fails() {
    let engine = engine_with_schema();
    let instance = ObjectInstance::new(
        "emp_1",
        "Employee",
        HashMap::from([("emp_id".to_string(), PropertyValue::Integer(1))]),
    );
    let err = engine.create_object_instance(instance).unwrap_err();
    match err {
        OntologyError::SchemaValidation { issues, .. } => {
            assert!(issues.contains(&ValidationIssue::MissingProperty {
                property: "name".into()
            }));
        }
        other => panic!("expected SchemaValidation, got {other:?}"),
    }
}

#[test]
fn create_instance_type_mismatch_fails() {
    let engine = engine_with_schema();
    let instance = ObjectInstance::new(
        "emp_1",
        "Employee",
        HashMap::from([
            (
                "emp_id".to_string(),
                PropertyValue::String("not-an-int".to_string()),
            ),
            ("name".to_string(), PropertyValue::String("X".to_string())),
        ]),
    );
    let err = engine.create_object_instance(instance).unwrap_err();
    match err {
        OntologyError::SchemaValidation { issues, .. } => {
            assert!(issues.iter().any(|i| matches!(
                i,
                ValidationIssue::TypeMismatch { property, .. } if property == "emp_id"
            )));
        }
        other => panic!("expected SchemaValidation, got {other:?}"),
    }
}

#[test]
fn create_instance_unknown_property_fails() {
    let engine = engine_with_schema();
    let instance = ObjectInstance::new(
        "emp_1",
        "Employee",
        HashMap::from([
            ("emp_id".to_string(), PropertyValue::Integer(1)),
            ("name".to_string(), PropertyValue::String("X".to_string())),
            (
                "nickname".to_string(),
                PropertyValue::String("Y".to_string()),
            ),
        ]),
    );
    let err = engine.create_object_instance(instance).unwrap_err();
    match err {
        OntologyError::SchemaValidation { issues, .. } => {
            assert!(issues.contains(&ValidationIssue::UnknownProperty {
                property: "nickname".into()
            }));
        }
        other => panic!("expected SchemaValidation, got {other:?}"),
    }
}

#[test]
fn duplicate_instance_id_fails() {
    let engine = engine_with_schema();
    engine.create_object_instance(pilot("emp_1", 1, "X")).unwrap();
    let err = engine
        .create_object_instance(pilot("emp_1", 2, "Y"))
        .unwrap_err();
    assert_eq!(err, OntologyError::DuplicateInstance("emp_1".into()));
}

#[test]
fn link_creation_and_traversal_both_directions() {
    let engine = engine_with_schema();
    engine.create_object_instance(pilot("emp_1", 1, "Maverick")).unwrap();
    engine
        .create_object_instance(aircraft("plane_1", "N140TG", "F-14 Tomcat"))
        .unwrap();
    engine
        .create_link(LinkInstance::new("flies", "emp_1", "plane_1"))
        .unwrap();

    let forward = engine.traverse_link("emp_1", "flies");
    assert_eq!(forward.len(), 1);
    assert_eq!(forward[0].id, "plane_1");

    let backward = engine.traverse("plane_1", "flies", Direction::Incoming);
    assert_eq!(backward.len(), 1);
    assert_eq!(backward[0].id, "emp_1");
}

#[test]
fn link_with_wrong_source_type_fails() {
    let engine = engine_with_schema();
    engine
        .create_object_instance(aircraft("plane_1", "N140TG", "F-14"))
        .unwrap();
    engine
        .create_object_instance(aircraft("plane_2", "N999ZZ", "F-18"))
        .unwrap();
    let err = engine
        .create_link(LinkInstance::new("flies", "plane_1", "plane_2"))
        .unwrap_err();
    assert!(matches!(err, OntologyError::LinkSourceTypeMismatch { .. }));
}

#[test]
fn duplicate_link_is_rejected() {
    let engine = engine_with_schema();
    engine.create_object_instance(pilot("emp_1", 1, "Maverick")).unwrap();
    engine
        .create_object_instance(aircraft("plane_1", "N140TG", "F-14"))
        .unwrap();
    engine
        .create_link(LinkInstance::new("flies", "emp_1", "plane_1"))
        .unwrap();
    let err = engine
        .create_link(LinkInstance::new("flies", "emp_1", "plane_1"))
        .unwrap_err();
    assert!(matches!(err, OntologyError::DuplicateLink { .. }));
}

#[test]
fn deleting_instance_cascades_to_links() {
    let engine = engine_with_schema();
    engine.create_object_instance(pilot("emp_1", 1, "Maverick")).unwrap();
    engine
        .create_object_instance(aircraft("plane_1", "N140TG", "F-14"))
        .unwrap();
    engine
        .create_link(LinkInstance::new("flies", "emp_1", "plane_1"))
        .unwrap();

    assert_eq!(engine.link_count(), 1);
    let removed = engine.delete_object_instance("emp_1").unwrap();
    assert_eq!(removed, 1);
    assert_eq!(engine.link_count(), 0);
    assert!(engine.get_object_instance("emp_1").is_none());
    assert!(engine.traverse_link("emp_1", "flies").is_empty());
}

#[test]
fn update_properties_validates_and_merges() {
    let engine = engine_with_schema();
    engine.create_object_instance(pilot("emp_1", 1, "Maverick")).unwrap();

    engine
        .update_object_properties(
            "emp_1",
            HashMap::from([("name".to_string(), PropertyValue::String("Pete".to_string()))]),
        )
        .unwrap();

    let updated = engine.get_object_instance("emp_1").unwrap();
    assert_eq!(
        updated.properties.get("name"),
        Some(&PropertyValue::String("Pete".to_string()))
    );
    // emp_id untouched
    assert_eq!(updated.properties.get("emp_id"), Some(&PropertyValue::Integer(1)));
}

#[test]
fn update_cannot_change_primary_key() {
    let engine = engine_with_schema();
    engine.create_object_instance(pilot("emp_1", 1, "Maverick")).unwrap();
    let err = engine
        .update_object_properties(
            "emp_1",
            HashMap::from([("emp_id".to_string(), PropertyValue::Integer(999))]),
        )
        .unwrap_err();
    assert!(matches!(err, OntologyError::ImmutablePrimaryKey { .. }));
}

#[test]
fn find_by_property_filters_correctly() {
    let engine = engine_with_schema();
    engine.create_object_instance(pilot("emp_1", 1, "Maverick")).unwrap();
    engine.create_object_instance(pilot("emp_2", 2, "Goose")).unwrap();

    let found = engine.find_by_property(
        "Employee",
        "name",
        &PropertyValue::String("Goose".to_string()),
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "emp_2");
}

#[test]
fn concurrent_reads_and_writes_are_consistent() {
    let engine = Arc::new(engine_with_schema());
    engine
        .create_object_instance(aircraft("plane_1", "N140TG", "F-14"))
        .unwrap();

    let mut handles = Vec::new();
    for i in 0..20 {
        let engine = Arc::clone(&engine);
        handles.push(thread::spawn(move || {
            let id = format!("emp_{i}");
            engine
                .create_object_instance(pilot(&id, i, "Name"))
                .unwrap();
            engine
                .create_link(LinkInstance::new("flies", &id, "plane_1"))
                .unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(engine.instance_count(), 21); // 20 pilots + 1 aircraft
    assert_eq!(engine.link_count(), 20);
    assert_eq!(
        engine.traverse("plane_1", "flies", Direction::Incoming).len(),
        20
    );
}

#[test]
fn object_type_builder_requires_primary_key_to_be_declared() {
    let result = ObjectTypeBuilder::new("Employee")
        .primary_key("emp_id")
        .property("name", PropertyType::String)
        .build();
    assert!(result.is_err());
}

#[test]
fn serde_roundtrip_for_property_value_and_instance() {
    let instance = pilot("emp_1", 1, "Maverick");
    let json = serde_json::to_string(&instance).unwrap();
    let deserialized: ObjectInstance = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.id, instance.id);
    assert_eq!(
        deserialized.properties.get("name"),
        instance.properties.get("name")
    );
}
