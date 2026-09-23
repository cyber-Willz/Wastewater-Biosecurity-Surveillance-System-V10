use ontology_engine::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn build_schema(engine: &OntologyEngine) -> Result<()> {
    let employee_type = ObjectTypeBuilder::new("Employee")
        .primary_key("emp_id")
        .property("emp_id", PropertyType::Integer)
        .property("name", PropertyType::String)
        .build()
        .map_err(|e| OntologyError::EmptyPrimaryKey { name: e })?;

    let aircraft_type = ObjectTypeBuilder::new("Aircraft")
        .primary_key("tail_number")
        .property("tail_number", PropertyType::String)
        .property("model", PropertyType::String)
        .build()
        .map_err(|e| OntologyError::EmptyPrimaryKey { name: e })?;

    engine.register_object_type(employee_type)?;
    engine.register_object_type(aircraft_type)?;
    engine.register_link_type(LinkType::new("flies", "Employee", "Aircraft"))?;
    Ok(())
}

fn seed_data(engine: &OntologyEngine) -> Result<()> {
    let pilot = ObjectInstance::new(
        "emp_101",
        "Employee",
        HashMap::from([
            ("emp_id".to_string(), PropertyValue::Integer(101)),
            (
                "name".to_string(),
                PropertyValue::String("Maverick".to_string()),
            ),
        ]),
    );

    let plane = ObjectInstance::new(
        "plane_f14",
        "Aircraft",
        HashMap::from([
            (
                "tail_number".to_string(),
                PropertyValue::String("N140TG".to_string()),
            ),
            (
                "model".to_string(),
                PropertyValue::String("F-14 Tomcat".to_string()),
            ),
        ]),
    );

    engine.create_object_instance(pilot)?;
    engine.create_object_instance(plane)?;
    engine.create_link(LinkInstance::new("flies", "emp_101", "plane_f14"))?;
    Ok(())
}

fn main() -> Result<()> {
    init_tracing();

    let engine = Arc::new(OntologyEngine::new());
    build_schema(&engine)?;
    seed_data(&engine)?;

    println!("--- Operational Ontology Query ---");
    for aircraft in engine.traverse_link("emp_101", "flies") {
        println!(
            "Pilot Maverick is cleared to fly: {}",
            aircraft.properties.get("model").unwrap()
        );
    }

    // Demonstrate reverse traversal via the backward index.
    for pilot in engine.traverse("plane_f14", "flies", Direction::Incoming) {
        println!(
            "Aircraft N140TG is flown by: {}",
            pilot.properties.get("name").unwrap()
        );
    }

    // Demonstrate that the engine is safe to share across threads: fire off
    // several concurrent readers and one writer registering a new pilot.
    let mut handles = Vec::new();
    for i in 0..4 {
        let engine = Arc::clone(&engine);
        handles.push(thread::spawn(move || {
            let count = engine.traverse_link("emp_101", "flies").len();
            tracing::debug!(reader = i, links_seen = count, "concurrent read");
        }));
    }
    {
        let engine = Arc::clone(&engine);
        handles.push(thread::spawn(move || {
            let goose = ObjectInstance::new(
                "emp_102",
                "Employee",
                HashMap::from([
                    ("emp_id".to_string(), PropertyValue::Integer(102)),
                    (
                        "name".to_string(),
                        PropertyValue::String("Goose".to_string()),
                    ),
                ]),
            );
            engine
                .create_object_instance(goose)
                .expect("concurrent insert should succeed");
        }));
    }
    for h in handles {
        h.join().expect("thread panicked");
    }

    println!(
        "Final state: {} instances, {} links",
        engine.instance_count(),
        engine.link_count()
    );

    // Demonstrate validation failure handling (missing + unknown property).
    let bad = ObjectInstance::new(
        "emp_bad",
        "Employee",
        HashMap::from([(
            "nickname".to_string(),
            PropertyValue::String("Iceman".to_string()),
        )]),
    );
    match engine.create_object_instance(bad) {
        Ok(()) => unreachable!("this instance is intentionally invalid"),
        Err(e) => println!("Expected validation error: {e}"),
    }

    Ok(())
}
