use std::sync::Arc;

use revision::revisioned;

use crate::{
    BlobId, ComponentOwner, EntityId, Error, ProjectId, Relation, RelationId, Result, Snapshot,
    component::{ComponentKey, StoredComponent},
    state::{State, StoredComponentEntry, load_components, store_components},
};

#[revisioned(revision = 1)]
#[derive(Clone, Debug)]
pub(crate) enum Operation {
    InsertPage {
        id: EntityId,
        position: u32,
        components: Vec<StoredComponentEntry>,
    },
    RemovePage {
        id: EntityId,
        position: u32,
        components: Vec<StoredComponentEntry>,
    },
    MovePage {
        id: EntityId,
        before: u32,
        after: u32,
    },
    InsertEntity {
        page: EntityId,
        id: EntityId,
        parent: EntityId,
        position: u32,
        components: Vec<StoredComponentEntry>,
    },
    RemoveEntity {
        page: EntityId,
        id: EntityId,
        parent: EntityId,
        position: u32,
        components: Vec<StoredComponentEntry>,
    },
    MoveEntity {
        id: EntityId,
        before_page: EntityId,
        before_parent: EntityId,
        before_position: u32,
        after_page: EntityId,
        after_parent: EntityId,
        after_position: u32,
    },
    ReplaceComponent {
        owner: ComponentOwner,
        key: ComponentKey,
        before: Option<StoredComponent>,
        after: Option<StoredComponent>,
    },
    InsertRelation {
        id: RelationId,
        value: Relation,
        components: Vec<StoredComponentEntry>,
    },
    RemoveRelation {
        id: RelationId,
        value: Relation,
        components: Vec<StoredComponentEntry>,
    },
    ReplaceRelation {
        id: RelationId,
        before: Relation,
        after: Relation,
    },
}

impl Operation {
    pub(crate) fn apply(&self, state: &mut State) -> Result<()> {
        match self {
            Self::InsertPage {
                id,
                position,
                components,
            } => state.insert_page(
                *id,
                *position as usize,
                load_components(components.clone())?,
            ),
            Self::RemovePage {
                id,
                position,
                components,
            } => {
                let (parent, actual) = state.parent_and_position(*id)?;
                if parent.is_some() || actual != *position as usize {
                    return Err(Error::PatchConflict("page position changed".to_owned()));
                }
                let page = state.page(*id)?;
                let current = store_components(&page.entities[page.root].components);
                if current != *components {
                    return Err(Error::PatchConflict("page components changed".to_owned()));
                }
                state.remove_page(*id).map(|_| ())
            }
            Self::MovePage { id, before, after } => {
                let (parent, actual) = state.parent_and_position(*id)?;
                if parent.is_some() || actual != *before as usize {
                    return Err(Error::PatchConflict("page position changed".to_owned()));
                }
                state.move_page(*id, *after as usize)
            }
            Self::InsertEntity {
                page,
                id,
                parent,
                position,
                components,
            } => state.insert_entity(
                *page,
                *id,
                *parent,
                *position as usize,
                load_components(components.clone())?,
            ),
            Self::RemoveEntity {
                page,
                id,
                parent,
                position,
                components,
            } => {
                if state.page_for(*id)? != *page {
                    return Err(Error::PatchConflict("entity changed pages".to_owned()));
                }
                let (actual_parent, actual_position) = state.parent_and_position(*id)?;
                if actual_parent != Some(*parent) || actual_position != *position as usize {
                    return Err(Error::PatchConflict("entity position changed".to_owned()));
                }
                let current = store_components(&state.entity(*id)?.components);
                if current != *components {
                    return Err(Error::PatchConflict("entity components changed".to_owned()));
                }
                state.remove_leaf(*id).map(|_| ())
            }
            Self::MoveEntity {
                id,
                before_page,
                before_parent,
                before_position,
                after_page,
                after_parent,
                after_position,
            } => {
                if state.page_for(*id)? != *before_page
                    || state.page_for(*before_parent)? != *before_page
                    || state.page_for(*after_parent)? != *after_page
                {
                    return Err(Error::PatchConflict("entity changed pages".to_owned()));
                }
                let (parent, position) = state.parent_and_position(*id)?;
                if parent != Some(*before_parent) || position != *before_position as usize {
                    return Err(Error::PatchConflict("entity position changed".to_owned()));
                }
                state.move_entity(*id, *after_parent, *after_position as usize)
            }
            Self::ReplaceComponent {
                owner,
                key,
                before,
                after,
            } => {
                let current = component(state, *owner, key)?.map(|value| value.to_stored());
                if current != *before {
                    return Err(Error::PatchConflict(format!(
                        "component {} changed",
                        key.kind
                    )));
                }
                replace_component(state, *owner, key.clone(), after.clone()).map(|_| ())
            }
            Self::InsertRelation {
                id,
                value,
                components,
            } => {
                state.insert_relation(*id, value.clone())?;
                for entry in components {
                    state.set_relation_component(
                        *id,
                        entry.key.clone(),
                        crate::component::ComponentRecord::from_stored(entry.value.clone())?,
                    )?;
                }
                Ok(())
            }
            Self::RemoveRelation {
                id,
                value,
                components,
            } => {
                let current = state
                    .relations
                    .get(id)
                    .ok_or(Error::RelationNotFound(*id))?;
                if current.value != *value || store_components(&current.components) != *components {
                    return Err(Error::PatchConflict("relation changed".to_owned()));
                }
                state.remove_relation(*id).map(|_| ())
            }
            Self::ReplaceRelation { id, before, after } => {
                if state
                    .relations
                    .get(id)
                    .ok_or(Error::RelationNotFound(*id))?
                    .value
                    != *before
                {
                    return Err(Error::PatchConflict("relation changed".to_owned()));
                }
                state.set_relation_value(*id, after.clone()).map(|_| ())
            }
        }
    }

    pub(crate) fn reversed(&self) -> Self {
        match self {
            Self::InsertPage {
                id,
                position,
                components,
            } => Self::RemovePage {
                id: *id,
                position: *position,
                components: components.clone(),
            },
            Self::RemovePage {
                id,
                position,
                components,
            } => Self::InsertPage {
                id: *id,
                position: *position,
                components: components.clone(),
            },
            Self::MovePage { id, before, after } => Self::MovePage {
                id: *id,
                before: *after,
                after: *before,
            },
            Self::InsertEntity {
                page,
                id,
                parent,
                position,
                components,
            } => Self::RemoveEntity {
                page: *page,
                id: *id,
                parent: *parent,
                position: *position,
                components: components.clone(),
            },
            Self::RemoveEntity {
                page,
                id,
                parent,
                position,
                components,
            } => Self::InsertEntity {
                page: *page,
                id: *id,
                parent: *parent,
                position: *position,
                components: components.clone(),
            },
            Self::MoveEntity {
                id,
                before_page,
                before_parent,
                before_position,
                after_page,
                after_parent,
                after_position,
            } => Self::MoveEntity {
                id: *id,
                before_page: *after_page,
                before_parent: *after_parent,
                before_position: *after_position,
                after_page: *before_page,
                after_parent: *before_parent,
                after_position: *before_position,
            },
            Self::ReplaceComponent {
                owner,
                key,
                before,
                after,
            } => Self::ReplaceComponent {
                owner: *owner,
                key: key.clone(),
                before: after.clone(),
                after: before.clone(),
            },
            Self::InsertRelation {
                id,
                value,
                components,
            } => Self::RemoveRelation {
                id: *id,
                value: value.clone(),
                components: components.clone(),
            },
            Self::RemoveRelation {
                id,
                value,
                components,
            } => Self::InsertRelation {
                id: *id,
                value: value.clone(),
                components: components.clone(),
            },
            Self::ReplaceRelation { id, before, after } => Self::ReplaceRelation {
                id: *id,
                before: after.clone(),
                after: before.clone(),
            },
        }
    }
}

#[revisioned(revision = 1)]
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Observation {
    ProjectHierarchy {
        epoch: u64,
    },
    Page {
        page: EntityId,
        epoch: Option<u64>,
    },
    Children {
        parent: EntityId,
        children: Vec<EntityId>,
    },
    Component {
        owner: ComponentOwner,
        key: ComponentKey,
        fingerprint: Option<[u8; 32]>,
    },
}

impl Observation {
    fn validate(&self, state: &State) -> Result<()> {
        match self {
            Self::ProjectHierarchy { epoch } => {
                if state.page_order_epoch != *epoch {
                    return Err(Error::PatchConflict(
                        "observed project page order changed".to_owned(),
                    ));
                }
            }
            Self::Page { page, epoch } => {
                if state.pages.get(page).map(|page| page.epoch) != *epoch {
                    return Err(Error::PatchConflict(format!(
                        "observed page {page} changed"
                    )));
                }
            }
            Self::Children { parent, children } => {
                if !state.contains_entity(*parent) || state.child_ids(*parent)? != *children {
                    return Err(Error::PatchConflict(format!(
                        "children of entity {parent} changed"
                    )));
                }
            }
            Self::Component {
                owner,
                key,
                fingerprint,
            } => {
                let current = component(state, *owner, key)?.map(|value| value.fingerprint());
                if current != *fingerprint {
                    return Err(Error::PatchConflict(format!(
                        "observed component {} changed",
                        key.kind
                    )));
                }
            }
        }
        Ok(())
    }
}

#[revisioned(revision = 1)]
#[derive(Clone)]
struct PatchIdentity {
    project: koharu_storage::DocumentId,
    base: koharu_storage::Revision,
    observations: Vec<Observation>,
    operations: Vec<Operation>,
    attachments: Vec<BlobId>,
    label: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Patch {
    pub(crate) project: koharu_storage::DocumentId,
    pub(crate) base_revision: koharu_storage::Revision,
    pub(crate) base_state: Arc<State>,
    pub(crate) state: Arc<State>,
    pub(crate) observations: Arc<[Observation]>,
    pub(crate) operations: Arc<[Operation]>,
    pub(crate) attachments: Arc<[(BlobId, bytes::Bytes)]>,
    pub(crate) label: Option<Arc<str>>,
    fingerprint: koharu_storage::PatchId,
}

impl Patch {
    pub(crate) fn new(
        base: &Snapshot,
        state: State,
        observations: Vec<Observation>,
        operations: Vec<Operation>,
        attachments: Vec<(BlobId, bytes::Bytes)>,
        label: Option<Arc<str>>,
    ) -> Result<Self> {
        let identity = PatchIdentity {
            project: base.state.document,
            base: base.state.revision,
            observations: observations.clone(),
            operations: operations.clone(),
            attachments: attachments.iter().map(|(blob, _)| *blob).collect(),
            label: label.as_deref().map(str::to_owned),
        };
        let fingerprint = koharu_storage::PatchId::for_bytes(&revision::to_vec(&identity)?);
        Ok(Self {
            project: base.state.document,
            base_revision: base.state.revision,
            base_state: base.state.clone(),
            state: Arc::new(state),
            observations: Arc::from(observations),
            operations: Arc::from(operations),
            attachments: Arc::from(attachments),
            label,
            fingerprint,
        })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    #[must_use]
    pub fn with_label(mut self, label: impl Into<Arc<str>>) -> Self {
        self.label = Some(label.into());
        self.rehash();
        self
    }

    #[must_use]
    pub fn project_id(&self) -> ProjectId {
        ProjectId(self.project)
    }

    #[must_use]
    pub fn base_revision(&self) -> crate::Revision {
        self.base_revision
    }

    #[must_use]
    pub fn fingerprint(&self) -> crate::PatchId {
        self.fingerprint
    }

    pub fn validate_on(&self, snapshot: &Snapshot) -> Result<()> {
        self.rebase_on(snapshot).map(|_| ())
    }

    pub fn rebase_on(&self, snapshot: &Snapshot) -> Result<Self> {
        if self.project != snapshot.state.document {
            return Err(Error::invalid("patch belongs to another project"));
        }
        if Arc::ptr_eq(&self.base_state, &snapshot.state) {
            return Ok(self.clone());
        }
        for observation in self.observations.iter() {
            observation.validate(&snapshot.state)?;
        }
        let mut state = (*snapshot.state).clone();
        for operation in self.operations.iter() {
            operation.apply(&mut state)?;
        }
        Self::new(
            snapshot,
            state,
            self.observations.to_vec(),
            self.operations.to_vec(),
            self.attachments.to_vec(),
            self.label.clone(),
        )
    }

    fn rehash(&mut self) {
        let identity = PatchIdentity {
            project: self.project,
            base: self.base_revision,
            observations: self.observations.to_vec(),
            operations: self.operations.to_vec(),
            attachments: self.attachments.iter().map(|(blob, _)| *blob).collect(),
            label: self.label.as_deref().map(str::to_owned),
        };
        self.fingerprint = koharu_storage::PatchId::for_bytes(
            &revision::to_vec(&identity).expect("patch identity is serializable"),
        );
    }
}

/// A patch reduced to the bytes another process can carry.
///
/// [`Patch`] keeps the scene state it was built from, and that state owns the
/// storage session of the process that produced it. A task subprocess therefore
/// sends this envelope instead, and the parent rebuilds a [`Patch`] against its
/// own snapshot before committing. `stage` and `page` repeat the stage output
/// the patch belongs to, because the parent has to commit it before the label
/// is attached.
#[revisioned(revision = 1)]
#[derive(Clone)]
pub struct PatchTransfer {
    pub stage: String,
    pub page: EntityId,
    /// The document the patch applies to. A process that built the patch
    /// against a copy of the scene overwrites this with the original document
    /// before sending it, because the copy's own document would not match.
    pub project: koharu_storage::DocumentId,
    /// The revision the patch was computed against. The receiver rejects the
    /// transfer when this is not its current revision.
    pub base: koharu_storage::Revision,
    observations: Vec<Observation>,
    operations: Vec<Operation>,
    attachments: Vec<(BlobId, Vec<u8>)>,
    label: Option<String>,
}

impl Patch {
    pub fn to_transfer(&self, stage: impl Into<String>, page: EntityId) -> PatchTransfer {
        PatchTransfer {
            stage: stage.into(),
            page,
            project: self.project,
            base: self.base_revision,
            observations: self.observations.to_vec(),
            operations: self.operations.to_vec(),
            attachments: self
                .attachments
                .iter()
                .map(|(blob, bytes)| (*blob, bytes.to_vec()))
                .collect(),
            label: self.label.as_deref().map(str::to_owned),
        }
    }

    /// Rebuilds the patch so it applies to `snapshot`.
    ///
    /// The transfer names the revision it was computed against. Rebuilding
    /// fails when that revision is not the snapshot's, because applying the
    /// operations to a newer scene would silently drop the conflict checks the
    /// producing process already made.
    pub fn from_transfer(snapshot: &Snapshot, transfer: PatchTransfer) -> Result<Self> {
        if transfer.project != snapshot.state.document {
            return Err(Error::invalid("patch belongs to another project"));
        }
        if transfer.base != snapshot.state.revision {
            return Err(Error::Storage(koharu_storage::Error::RevisionConflict {
                current: snapshot.state.revision,
                proposed: transfer.base,
            }));
        }
        for observation in &transfer.observations {
            observation.validate(&snapshot.state)?;
        }
        let state = apply_operations(&snapshot.state, &transfer.operations)?;
        Self::new(
            snapshot,
            state,
            transfer.observations,
            transfer.operations,
            transfer
                .attachments
                .into_iter()
                .map(|(blob, bytes)| (blob, bytes::Bytes::from(bytes)))
                .collect(),
            transfer.label.map(Arc::from),
        )
    }
}

pub(crate) fn apply_operations(state: &State, operations: &[Operation]) -> Result<State> {
    let mut next = state.clone();
    for operation in operations {
        operation.apply(&mut next)?;
    }
    Ok(next)
}

fn component<'a>(
    state: &'a State,
    owner: ComponentOwner,
    key: &ComponentKey,
) -> Result<Option<&'a crate::component::ComponentRecord>> {
    match owner {
        ComponentOwner::Project => Ok(state.project_component(key)),
        ComponentOwner::Entity(id) => state.component(id, key),
        ComponentOwner::Relation(id) => state.relation_component(id, key),
    }
}

fn replace_component(
    state: &mut State,
    owner: ComponentOwner,
    key: ComponentKey,
    value: Option<StoredComponent>,
) -> Result<Option<crate::component::ComponentRecord>> {
    let value = value
        .map(crate::component::ComponentRecord::from_stored)
        .transpose()?;
    match (owner, value) {
        (ComponentOwner::Project, Some(value)) => Ok(state.set_project_component(key, value)),
        (ComponentOwner::Project, None) => Ok(state.remove_project_component(&key)),
        (ComponentOwner::Entity(id), Some(value)) => state.set_entity_component(id, key, value),
        (ComponentOwner::Entity(id), None) => state.remove_entity_component(id, &key),
        (ComponentOwner::Relation(id), Some(value)) => state.set_relation_component(id, key, value),
        (ComponentOwner::Relation(id), None) => state.remove_relation_component(id, &key),
    }
}
