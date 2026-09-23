//! The [`OntologyEngine`]: schema registry plus a live, indexed instance
//! graph.
//!
//! Design notes:
//! - Schema (`object_types`, `link_types`) rarely changes after startup and
//!   is kept in its own lock so read-heavy schema lookups never contend
//!   with the instance graph.
//! - All instance/link/index mutation lives behind a *single* lock
//!   (`Store`). Creating a link requires reading instances and writing the
//!   link list; if those were separate locks, a create-link and a
//!   delete-instance could race (TOCTOU) and leave a dangling link pointing
//!   at a deleted instance. A single lock makes every multi-step mutation
//!   atomic with respect to other mutations.
//! - Forward/backward adjacency indexes make `traverse` O(edges from that
//!   node) instead of O(total edges in the graph).

use crate::error::{OntologyError, Result, ValidationIssue};
use crate::types::{
    Direction, LinkInstance, LinkType, ObjectInstance, ObjectType, PropertyValue,
};
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};

type IndexKey = (String, String); // (node_id, link_type)

#[derive(Default)]
struct Store {
    instances: HashMap<String, ObjectInstance>,
    links: HashSet<LinkInstance>,
    /// (source_id, link_type) -> set of target_ids
    forward_index: HashMap<IndexKey, HashSet<String>>,
    /// (target_id, link_type) -> set of source_ids
    backward_index: HashMap<IndexKey, HashSet<String>>,
}

impl Store {
    fn insert_link(&mut self, link: LinkInstance) -> bool {
        if !self.links.insert(link.clone()) {
            return false;
        }
        self.forward_index
            .entry((link.source_id.clone(), link.link_type.clone()))
            .or_default()
            .insert(link.target_id.clone());
        self.backward_index
            .entry((link.target_id.clone(), link.link_type.clone()))
            .or_default()
            .insert(link.source_id.clone());
        true
    }

    fn remove_link(&mut self, link: &LinkInstance) -> bool {
        if !self.links.remove(link) {
            return false;
        }
        if let Some(set) = self
            .forward_index
            .get_mut(&(link.source_id.clone(), link.link_type.clone()))
        {
            set.remove(&link.target_id);
        }
        if let Some(set) = self
            .backward_index
            .get_mut(&(link.target_id.clone(), link.link_type.clone()))
        {
            set.remove(&link.source_id);
        }
        true
    }

    /// Remove every link touching `instance_id` (as source or target),
    /// keeping the indexes consistent. Returns the removed links.
    fn remove_links_touching(&mut self, instance_id: &str) -> Vec<LinkInstance> {
        let to_remove: Vec<LinkInstance> = self
            .links
            .iter()
            .filter(|l| l.source_id == instance_id || l.target_id == instance_id)
            .cloned()
            .collect();
        for link in &to_remove {
            self.remove_link(link);
        }
        to_remove
    }
}

/// Thread-safe ontology / knowledge-graph engine.
///
/// Cloning an [`OntologyEngine`] handle is not supported directly; wrap it
/// in `Arc<OntologyEngine>` (as `main` does) to share it across threads or
/// async tasks.
pub struct OntologyEngine {
    object_types: RwLock<HashMap<String, ObjectType>>,
    link_types: RwLock<HashMap<String, LinkType>>,
    store: RwLock<Store>,
}

impl Default for OntologyEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl OntologyEngine {
    pub fn new() -> Self {
        Self {
            object_types: RwLock::new(HashMap::new()),
            link_types: RwLock::new(HashMap::new()),
            store: RwLock::new(Store::default()),
        }
    }

    // ---------------------------------------------------------------
    // Schema management
    // ---------------------------------------------------------------

    #[tracing::instrument(skip(self, obj_type), fields(name = %obj_type.name))]
    pub fn register_object_type(&self, obj_type: ObjectType) -> Result<()> {
        if obj_type.primary_key.is_empty() {
            return Err(OntologyError::EmptyPrimaryKey {
                name: obj_type.name,
            });
        }
        let mut types = self.object_types.write();
        if types.contains_key(&obj_type.name) {
            return Err(OntologyError::ObjectTypeAlreadyRegistered(obj_type.name));
        }
        tracing::info!("registered object type");
        types.insert(obj_type.name.clone(), obj_type);
        Ok(())
    }

    #[tracing::instrument(skip(self, link_type), fields(name = %link_type.name))]
    pub fn register_link_type(&self, link_type: LinkType) -> Result<()> {
        {
            let object_types = self.object_types.read();
            if !object_types.contains_key(&link_type.source_type) {
                return Err(OntologyError::ObjectTypeNotRegistered(
                    link_type.source_type.clone(),
                ));
            }
            if !object_types.contains_key(&link_type.target_type) {
                return Err(OntologyError::ObjectTypeNotRegistered(
                    link_type.target_type.clone(),
                ));
            }
        }
        let mut types = self.link_types.write();
        if types.contains_key(&link_type.name) {
            return Err(OntologyError::LinkTypeAlreadyRegistered(link_type.name));
        }
        tracing::info!("registered link type");
        types.insert(link_type.name.clone(), link_type);
        Ok(())
    }

    pub fn get_object_type(&self, name: &str) -> Option<ObjectType> {
        self.object_types.read().get(name).cloned()
    }

    pub fn get_link_type(&self, name: &str) -> Option<LinkType> {
        self.link_types.read().get(name).cloned()
    }

    // ---------------------------------------------------------------
    // Validation
    // ---------------------------------------------------------------

    /// Validate `properties` against `schema`, collecting *every* problem
    /// found rather than stopping at the first one.
    fn validate_properties(
        schema: &ObjectType,
        properties: &HashMap<String, PropertyValue>,
    ) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();

        for (prop_name, expected_type) in &schema.property_schemas {
            match properties.get(prop_name) {
                None => issues.push(ValidationIssue::MissingProperty {
                    property: prop_name.clone(),
                }),
                Some(value) => {
                    let found = value.type_of();
                    if found != *expected_type {
                        issues.push(ValidationIssue::TypeMismatch {
                            property: prop_name.clone(),
                            expected: expected_type.to_string(),
                            found: found.to_string(),
                        });
                    }
                }
            }
        }

        for prop_name in properties.keys() {
            if !schema.property_schemas.contains_key(prop_name) {
                issues.push(ValidationIssue::UnknownProperty {
                    property: prop_name.clone(),
                });
            }
        }

        issues
    }

    // ---------------------------------------------------------------
    // Instance management
    // ---------------------------------------------------------------

    #[tracing::instrument(skip(self, instance), fields(id = %instance.id, object_type = %instance.object_type))]
    pub fn create_object_instance(&self, instance: ObjectInstance) -> Result<()> {
        let schema = self
            .object_types
            .read()
            .get(&instance.object_type)
            .cloned()
            .ok_or_else(|| OntologyError::ObjectTypeNotRegistered(instance.object_type.clone()))?;

        let issues = Self::validate_properties(&schema, &instance.properties);
        if !issues.is_empty() {
            return Err(OntologyError::SchemaValidation {
                instance_id: instance.id,
                issue_count: issues.len(),
                issues,
            });
        }

        let mut store = self.store.write();
        if store.instances.contains_key(&instance.id) {
            return Err(OntologyError::DuplicateInstance(instance.id));
        }
        tracing::debug!("created object instance");
        store.instances.insert(instance.id.clone(), instance);
        Ok(())
    }

    /// Partially update an existing instance's properties. The primary key
    /// property cannot be changed. All supplied properties are re-validated
    /// against the schema before anything is written.
    #[tracing::instrument(skip(self, updates), fields(id = %instance_id))]
    pub fn update_object_properties(
        &self,
        instance_id: &str,
        updates: HashMap<String, PropertyValue>,
    ) -> Result<()> {
        let mut store = self.store.write();
        let existing = store
            .instances
            .get(instance_id)
            .ok_or_else(|| OntologyError::InstanceNotFound(instance_id.to_string()))?;

        let schema = self
            .object_types
            .read()
            .get(&existing.object_type)
            .cloned()
            .ok_or_else(|| OntologyError::ObjectTypeNotRegistered(existing.object_type.clone()))?;

        if let Some(pk_update) = updates.get(&schema.primary_key) {
            let current_pk = existing.properties.get(&schema.primary_key);
            if current_pk != Some(pk_update) {
                return Err(OntologyError::ImmutablePrimaryKey {
                    instance_id: instance_id.to_string(),
                    property: schema.primary_key.clone(),
                });
            }
        }

        let mut merged = existing.properties.clone();
        for (k, v) in &updates {
            merged.insert(k.clone(), v.clone());
        }

        let issues = Self::validate_properties(&schema, &merged);
        if !issues.is_empty() {
            return Err(OntologyError::SchemaValidation {
                instance_id: instance_id.to_string(),
                issue_count: issues.len(),
                issues,
            });
        }

        let instance = store.instances.get_mut(instance_id).unwrap();
        instance.properties = merged;
        tracing::debug!(count = updates.len(), "updated object instance properties");
        Ok(())
    }

    pub fn get_object_instance(&self, id: &str) -> Option<ObjectInstance> {
        self.store.read().instances.get(id).cloned()
    }

    pub fn list_instances_by_type(&self, object_type: &str) -> Vec<ObjectInstance> {
        self.store
            .read()
            .instances
            .values()
            .filter(|i| i.object_type == object_type)
            .cloned()
            .collect()
    }

    /// Return every instance of `object_type` whose `property` equals `value`.
    pub fn find_by_property(
        &self,
        object_type: &str,
        property: &str,
        value: &PropertyValue,
    ) -> Vec<ObjectInstance> {
        self.store
            .read()
            .instances
            .values()
            .filter(|i| {
                i.object_type == object_type && i.properties.get(property) == Some(value)
            })
            .cloned()
            .collect()
    }

    /// Delete an instance and cascade-delete every link that touches it.
    /// Returns the number of links removed as a side effect.
    #[tracing::instrument(skip(self), fields(id = %instance_id))]
    pub fn delete_object_instance(&self, instance_id: &str) -> Result<usize> {
        let mut store = self.store.write();
        if store.instances.remove(instance_id).is_none() {
            return Err(OntologyError::InstanceNotFound(instance_id.to_string()));
        }
        let removed = store.remove_links_touching(instance_id);
        tracing::debug!(cascaded_links = removed.len(), "deleted object instance");
        Ok(removed.len())
    }

    // ---------------------------------------------------------------
    // Link management
    // ---------------------------------------------------------------

    #[tracing::instrument(skip(self, link), fields(link_type = %link.link_type, source = %link.source_id, target = %link.target_id))]
    pub fn create_link(&self, link: LinkInstance) -> Result<()> {
        let schema = self
            .link_types
            .read()
            .get(&link.link_type)
            .cloned()
            .ok_or_else(|| OntologyError::LinkTypeNotRegistered(link.link_type.clone()))?;

        let mut store = self.store.write();

        let source = store
            .instances
            .get(&link.source_id)
            .ok_or_else(|| OntologyError::InstanceNotFound(link.source_id.clone()))?;
        if source.object_type != schema.source_type {
            return Err(OntologyError::LinkSourceTypeMismatch {
                link_type: link.link_type.clone(),
                expected_source: schema.source_type.clone(),
                actual_source: source.object_type.clone(),
                source_id: link.source_id.clone(),
            });
        }

        let target = store
            .instances
            .get(&link.target_id)
            .ok_or_else(|| OntologyError::InstanceNotFound(link.target_id.clone()))?;
        if target.object_type != schema.target_type {
            return Err(OntologyError::LinkTargetTypeMismatch {
                link_type: link.link_type.clone(),
                expected_target: schema.target_type.clone(),
                actual_target: target.object_type.clone(),
                target_id: link.target_id.clone(),
            });
        }

        if !store.insert_link(link.clone()) {
            return Err(OntologyError::DuplicateLink {
                link_type: link.link_type,
                source_id: link.source_id,
                target_id: link.target_id,
            });
        }
        tracing::debug!("created link");
        Ok(())
    }

    #[tracing::instrument(skip(self, link), fields(link_type = %link.link_type, source = %link.source_id, target = %link.target_id))]
    pub fn delete_link(&self, link: &LinkInstance) -> Result<()> {
        let mut store = self.store.write();
        if !store.remove_link(link) {
            return Err(OntologyError::DuplicateLink {
                link_type: link.link_type.clone(),
                source_id: link.source_id.clone(),
                target_id: link.target_id.clone(),
            });
        }
        tracing::debug!("deleted link");
        Ok(())
    }

    /// Traverse from `node_id` along `link_type` in the given `direction`,
    /// returning the connected instances. O(degree) via the adjacency index
    /// rather than a full scan of all links.
    pub fn traverse(
        &self,
        node_id: &str,
        link_type: &str,
        direction: Direction,
    ) -> Vec<ObjectInstance> {
        let store = self.store.read();
        let index = match direction {
            Direction::Outgoing => &store.forward_index,
            Direction::Incoming => &store.backward_index,
        };
        let Some(ids) = index.get(&(node_id.to_string(), link_type.to_string())) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|id| store.instances.get(id).cloned())
            .collect()
    }

    /// Convenience wrapper preserving the original API's outgoing-only
    /// traversal semantics.
    pub fn traverse_link(&self, source_id: &str, link_type_name: &str) -> Vec<ObjectInstance> {
        self.traverse(source_id, link_type_name, Direction::Outgoing)
    }

    pub fn instance_count(&self) -> usize {
        self.store.read().instances.len()
    }

    pub fn link_count(&self) -> usize {
        self.store.read().links.len()
    }
}
