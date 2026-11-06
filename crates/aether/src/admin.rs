use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use aether_core::Grant;
use aether_store::{
    AuditActor, AuditResult, ChangeKind, ManagedMember, NodeStore, NodeSummary,
    StoreError, TaskActivity,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum AdminError {
    #[error("change field is invalid: {0}")]
    InvalidField(&'static str),
    #[error("pending change has expired")]
    Expired,
    #[error("local UI confirmation is required")]
    ConfirmationRequired,
    #[error("administration store is unavailable")]
    Store,
    #[error("task cancellation failed")]
    Task,
    #[error("this node does not support live application of {0} changes")]
    Unsupported(&'static str),
    #[error(
        "policy changed since this change was queued; it can no longer be confirmed"
    )]
    Stale,
}

impl schemars::JsonSchema for ChangeRequest {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ChangeRequest".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "A typed administration change."
        })
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ChangeRequest {
    Workspace { id: String, path: String },
    Grant { id: String, grant: Grant },
    Directory { id: String, url: String },
    Capability { id: String, capability: String },
    Limit { id: aether_core::limits::QueueLimitId, limit: u32 },
}

impl<'de> Deserialize<'de> for ChangeRequest {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum Raw {
            Workspace { id: String, path: String },
            Grant { id: String, grant: String },
            Directory { id: String, url: String },
            Capability { id: String, capability: String },
            Limit { id: String, limit: u32 },
        }
        match Raw::deserialize(deserializer)? {
            Raw::Workspace { id, path } => Ok(Self::Workspace { id, path }),
            Raw::Grant { id, grant } => {
                let grant = crate::identity::decode_grant(&grant)
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Grant { id, grant })
            }
            Raw::Directory { id, url } => Ok(Self::Directory { id, url }),
            Raw::Capability { id, capability } => {
                Ok(Self::Capability { id, capability })
            }
            Raw::Limit { id, limit } => {
                let id = aether_core::limits::QueueLimitId::parse(&id)
                    .ok_or_else(|| serde::de::Error::custom("unknown limit id"))?;
                Ok(Self::Limit { id, limit })
            }
        }
    }
}

impl Serialize for ChangeRequest {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum Raw<'a> {
            Workspace { id: &'a str, path: &'a str },
            Grant { id: &'a str, grant: String },
            Directory { id: &'a str, url: &'a str },
            Capability { id: &'a str, capability: &'a str },
            Limit { id: &'a str, limit: u32 },
        }
        match self {
            Self::Workspace { id, path } => {
                Raw::Workspace { id, path }.serialize(serializer)
            }
            Self::Grant { id, grant } => {
                let grant = crate::identity::encode_grant(grant)
                    .map_err(serde::ser::Error::custom)?;
                Raw::Grant { id, grant }.serialize(serializer)
            }
            Self::Directory { id, url } => {
                Raw::Directory { id, url }.serialize(serializer)
            }
            Self::Capability { id, capability } => {
                Raw::Capability { id, capability }.serialize(serializer)
            }
            Self::Limit { id, limit } => {
                Raw::Limit { id: id.as_str(), limit: *limit }.serialize(serializer)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ApplyOutcome {
    Applied,
    Pending,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ChangeOutcome {
    Validated,
    Applied,
    Pending,
}

/// A validated change, with the time after which it cannot be confirmed.
pub struct Change {
    request: ChangeRequest,
    expires_at: SystemTime,
}

impl ChangeRequest {
    pub fn into_change(self) -> Result<Change, AdminError> {
        let expires_at = SystemTime::now()
            .checked_add(std::time::Duration::from_secs(300))
            .ok_or(AdminError::InvalidField("expires_at"))?;
        if matches!(self, Self::Limit { limit: 0, .. }) {
            return Err(AdminError::InvalidField("limit"));
        }
        valid(self.id(), "id")?;
        match &self {
            Self::Workspace { path, .. } => valid(path, "path")?,
            Self::Grant { id, grant } => {
                valid(id, "id")?;
                if grant.id.as_str() != id {
                    return Err(AdminError::InvalidField("id"));
                }
            }
            Self::Directory { url, .. } => {
                if !url.starts_with("https://") {
                    return Err(AdminError::InvalidField("url"));
                }
            }
            Self::Capability { capability, .. } => valid(capability, "capability")?,
            Self::Limit { .. } => {}
        }
        Ok(Change { request: self, expires_at })
    }

    /// Rebuilds a request from the value that the store keeps for a pending
    /// change.
    fn from_stored(
        kind: ChangeKind,
        id: String,
        value: String,
    ) -> Result<Self, AdminError> {
        match kind {
            ChangeKind::Workspace => Ok(Self::Workspace { id, path: value }),
            ChangeKind::Grant => {
                let grant = crate::identity::decode_grant(&value)
                    .map_err(|_| AdminError::InvalidField("grant"))?;
                Ok(Self::Grant { id, grant })
            }
            ChangeKind::Directory => Ok(Self::Directory { id, url: value }),
            ChangeKind::Capability => Ok(Self::Capability { id, capability: value }),
            ChangeKind::Limit => {
                let limit_id = aether_core::limits::QueueLimitId::parse(&id)
                    .ok_or(AdminError::InvalidField("id"))?;
                value
                    .parse::<u32>()
                    .map(|limit| Self::Limit { id: limit_id, limit })
                    .map_err(|_| AdminError::InvalidField("limit"))
            }
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Workspace { id, .. }
            | Self::Grant { id, .. }
            | Self::Directory { id, .. }
            | Self::Capability { id, .. } => id,
            Self::Limit { id, .. } => id.as_str(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ChangeKind {
        match self {
            Self::Workspace { .. } => ChangeKind::Workspace,
            Self::Grant { .. } => ChangeKind::Grant,
            Self::Directory { .. } => ChangeKind::Directory,
            Self::Capability { .. } => ChangeKind::Capability,
            Self::Limit { .. } => ChangeKind::Limit,
        }
    }

    #[allow(clippy::expect_used)]
    fn value(&self) -> String {
        match self {
            Self::Workspace { path: value, .. }
            | Self::Directory { url: value, .. }
            | Self::Capability { capability: value, .. } => value.clone(),
            Self::Grant { grant, .. } => crate::identity::encode_grant(grant)
                .expect("a validated grant must encode"),
            Self::Limit { limit, .. } => limit.to_string(),
        }
    }
}

fn valid(value: &str, field: &'static str) -> Result<(), AdminError> {
    if value.is_empty() || value.contains('\0') {
        Err(AdminError::InvalidField(field))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PendingChange {
    pub id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuditEntry {
    pub actor: String,
    pub action: String,
    pub result: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum AdminStoreAccess {
    Ready,
    Unavailable,
}

pub struct Administration {
    revision: std::sync::atomic::AtomicU64,
    store: NodeStore,
    /// The running node; none in a process without one.
    live: Option<Arc<dyn LiveNode>>,
    /// Marked after each call that can write an audit record.
    audited: tokio::sync::watch::Sender<()>,
}

pub type SharedAdministration = Arc<Mutex<Administration>>;

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum GrantApplyError {
    #[error("the node refused the grant")]
    Refused,
}

pub trait GrantApplicator: Send + Sync {
    fn apply_signed_grant(
        &self,
        grant: aether_core::Grant,
    ) -> Result<(), GrantApplyError>;
    fn revoke_signed_grant(&self, grant_id: &str) -> Result<(), GrantApplyError>;
}

impl GrantApplicator for SharedAdministration {
    fn apply_signed_grant(
        &self,
        grant: aether_core::Grant,
    ) -> Result<(), GrantApplyError> {
        use std::sync::PoisonError;
        let id = grant.id.as_str().to_owned();
        let change = ChangeRequest::Grant { id, grant }
            .into_change()
            .map_err(|_| GrantApplyError::Refused)?;
        self.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .apply_now(&change, aether_store::AuditActor::Ui)
            .map_err(|_| GrantApplyError::Refused)
    }

    fn revoke_signed_grant(&self, grant_id: &str) -> Result<(), GrantApplyError> {
        use std::sync::PoisonError;
        self.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .revoke_inner(grant_id, aether_store::AuditActor::Ui)
            .map_err(|_| GrantApplyError::Refused)
    }
}

/// The running node that `Administration` applies changes to.
pub trait LiveNode: Send + Sync {
    fn cancel_task(&self, task_id: &str) -> Result<(), AdminError>;

    fn queue_limit(
        &self,
        id: aether_core::limits::QueueLimitId,
    ) -> Result<u32, AdminError>;

    fn set_queue_limit(
        &self,
        id: aether_core::limits::QueueLimitId,
        limit: u32,
    ) -> Result<(), AdminError>;

    /// Takes the requested workspace id and path together so the node can
    /// check the id against a policy-approved set before switching to the
    /// path — see `NodeService::approved_workspace`.
    fn apply_workspace(&self, id: &str, path: &str) -> Result<(), AdminError>;

    fn workspace(&self) -> Option<String>;

    fn apply_grant(&self, id: &str, grant: &str) -> Result<(), AdminError>;

    fn revoke_grant(&self, id: &str) -> Result<(), AdminError>;

    fn grants(&self) -> Result<Vec<aether_core::Grant>, AdminError>;

    /// Triggers a real publish/renew of the node's already-configured,
    /// already-pinned directory record. The node itself is responsible for
    /// rejecting a `url` that does not match the configured directory: this
    /// action is "renew now" for the pinned directory, not "switch to a new,
    /// unpinned one" — accepting an arbitrary URL here with no certificate to
    /// verify it against would let a caller redirect directory trust.
    fn renew_directory(&self, url: &str) -> Result<(), AdminError>;

    /// Publishes the pinned directory record with this capability replacing
    /// the current one — the only concrete meaning "capability publication"
    /// has today, since directory records are the sole vehicle for it and no
    /// separate capability-record primitive exists.
    fn publish_capability(&self, capability: &str) -> Result<(), AdminError>;

    fn set_relay(&self, relay: Option<String>) -> Result<(), AdminError>;

    /// Starts a round of the node's outbox at once.
    fn wake_outbox(&self) -> Result<(), AdminError>;

    /// The ids of tasks as they change; none when the node cannot tell.
    fn subscribe_events(&self) -> Option<tokio::sync::broadcast::Receiver<String>> {
        None
    }
}

#[must_use]
pub fn shared_with_store(store: NodeStore) -> SharedAdministration {
    Arc::new(Mutex::new(Administration::with_store(store)))
}

/// An administration over the node store in `directory`, for tests.
#[cfg(test)]
pub mod test_support {
    use std::sync::Mutex;

    use super::{AdminError, LiveNode, SharedAdministration, shared_with_store};

    /// # Panics
    ///
    /// Panics when the store does not open.
    #[must_use]
    pub fn shared_in(directory: &std::path::Path) -> SharedAdministration {
        shared_with_store(
            aether_store::NodeStore::in_dir(directory).expect("node store must open"),
        )
    }

    /// A live node that records what the administration asks of it.
    #[derive(Default)]
    pub struct FakeNode {
        calls: Mutex<Vec<String>>,
        granted: Mutex<Vec<String>>,
        workspace: Mutex<Option<String>>,
        existing_workspaces: bool,
        queue_limit: Option<u32>,
        directory: Option<String>,
        failure: Mutex<Option<AdminError>>,
        outbox: tokio::sync::Notify,
    }

    impl FakeNode {
        /// Every queue limit reads as `limit`. Without one, a read fails.
        #[must_use]
        pub const fn with_queue_limit(mut self, limit: u32) -> Self {
            self.queue_limit = Some(limit);
            self
        }

        /// The node renews only the directory at `url`. Without one, a
        /// directory change is unsupported, as on a node that publishes
        /// no record.
        #[must_use]
        pub fn with_directory(mut self, url: &str) -> Self {
            self.directory = Some(url.to_owned());
            self
        }

        /// The node starts on workspace `path`, and, like the node, it
        /// switches only to an existing directory.
        #[must_use]
        pub fn with_workspace(mut self, path: &str) -> Self {
            self.workspace = Mutex::new(Some(path.to_owned()));
            self.existing_workspaces = true;
            self
        }

        /// Every later change fails with `error`.
        ///
        /// # Panics
        ///
        /// Panics when the lock is poisoned.
        pub fn fail_with(&self, error: AdminError) {
            *self.failure.lock().expect("failure must lock") = Some(error);
        }

        /// Each call, as its operation and arguments.
        ///
        /// # Panics
        ///
        /// Panics when the lock is poisoned.
        #[must_use]
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("calls must lock").clone()
        }

        /// The grants applied to the node, revoked or not.
        ///
        /// # Panics
        ///
        /// Panics when the lock is poisoned.
        #[must_use]
        pub fn granted(&self) -> Vec<String> {
            self.granted.lock().expect("grants must lock").clone()
        }

        /// What [`LiveNode::wake_outbox`] notifies.
        #[must_use]
        pub const fn outbox(&self) -> &tokio::sync::Notify {
            &self.outbox
        }

        /// Records `call`, then fails with the error of [`Self::fail_with`].
        fn change(&self, call: String) -> Result<(), AdminError> {
            self.calls.lock().expect("calls must lock").push(call);
            self.failure.lock().expect("failure must lock").clone().map_or(Ok(()), Err)
        }

        fn revoked(&self) -> Vec<String> {
            self.calls()
                .iter()
                .filter_map(|call| call.strip_prefix("revoke "))
                .map(str::to_owned)
                .collect()
        }
    }

    impl LiveNode for FakeNode {
        fn cancel_task(&self, task_id: &str) -> Result<(), AdminError> {
            self.change(format!("cancel {task_id}"))
        }

        fn queue_limit(
            &self,
            _id: aether_core::limits::QueueLimitId,
        ) -> Result<u32, AdminError> {
            self.queue_limit.ok_or(AdminError::InvalidField("id"))
        }

        fn set_queue_limit(
            &self,
            id: aether_core::limits::QueueLimitId,
            limit: u32,
        ) -> Result<(), AdminError> {
            self.change(format!("limit {} {limit}", id.as_str()))
        }

        fn apply_workspace(&self, id: &str, path: &str) -> Result<(), AdminError> {
            if self.existing_workspaces && !std::path::Path::new(path).is_dir() {
                return Err(AdminError::InvalidField("path"));
            }
            self.change(format!("workspace {id} {path}"))?;
            *self.workspace.lock().expect("workspace must lock") =
                Some(path.to_owned());
            Ok(())
        }

        fn workspace(&self) -> Option<String> {
            self.workspace.lock().expect("workspace must lock").clone()
        }

        fn apply_grant(&self, id: &str, grant: &str) -> Result<(), AdminError> {
            self.change(format!("grant {id}"))?;
            self.granted.lock().expect("grants must lock").push(grant.to_owned());
            Ok(())
        }

        fn revoke_grant(&self, id: &str) -> Result<(), AdminError> {
            self.change(format!("revoke {id}"))
        }

        fn grants(&self) -> Result<Vec<aether_core::Grant>, AdminError> {
            let revoked = self.revoked();
            Ok(self
                .granted()
                .iter()
                .filter_map(|grant| crate::identity::decode_grant(grant).ok())
                .filter(|grant| !revoked.iter().any(|id| id == grant.id.as_str()))
                .collect())
        }

        fn renew_directory(&self, url: &str) -> Result<(), AdminError> {
            match &self.directory {
                None => Err(AdminError::Unsupported("directory")),
                Some(directory) if directory != url => {
                    Err(AdminError::InvalidField("url"))
                }
                Some(_) => self.change(format!("directory {url}")),
            }
        }

        fn publish_capability(&self, capability: &str) -> Result<(), AdminError> {
            self.change(format!("capability {capability}"))
        }

        fn set_relay(&self, relay: Option<String>) -> Result<(), AdminError> {
            self.change(format!("relay {}", relay.as_deref().unwrap_or("off")))
        }

        fn wake_outbox(&self) -> Result<(), AdminError> {
            self.outbox.notify_one();
            Ok(())
        }
    }
}
impl Administration {
    /// Resumes the durable admin revision, so that a pending change queued
    /// before a restart is still found stale. An unreadable revision counts
    /// as 0, as on a node that never applied a change.
    #[must_use]
    pub fn with_store(store: NodeStore) -> Self {
        let revision = store.admin_revision().unwrap_or(0);
        Self {
            revision: std::sync::atomic::AtomicU64::new(revision),
            store,
            live: None,
            audited: tokio::sync::watch::Sender::new(()),
        }
    }

    /// Marks each later call that can write an audit record.
    #[must_use]
    pub fn watch_audit(&self) -> tokio::sync::watch::Receiver<()> {
        self.audited.subscribe()
    }

    /// The ids of the running node's tasks as they change.
    #[must_use]
    pub fn subscribe_task_events(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<String>> {
        self.live.as_ref().and_then(|live| live.subscribe_events())
    }

    fn audited<R>(&self, result: R) -> R {
        self.audited.send_replace(());
        result
    }

    pub fn set_live(&mut self, live: Arc<dyn LiveNode>) {
        self.live = Some(live);
    }

    pub fn submit(
        &mut self,
        change: &Change,
        actor: AuditActor,
        dry_run: bool,
    ) -> Result<ChangeOutcome, AdminError> {
        if dry_run {
            return Ok(ChangeOutcome::Validated);
        }
        let result = self.apply(change, actor).map(|outcome| match outcome {
            ApplyOutcome::Applied => ChangeOutcome::Applied,
            ApplyOutcome::Pending => ChangeOutcome::Pending,
        });
        self.audited(result)
    }

    pub fn apply(
        &mut self,
        change: &Change,
        actor: AuditActor,
    ) -> Result<ApplyOutcome, AdminError> {
        if self.change_requires_confirmation(change, actor) {
            self.queue_pending(change)?;
            Ok(ApplyOutcome::Pending)
        } else {
            self.apply_now(change, actor)?;
            Ok(ApplyOutcome::Applied)
        }
    }

    fn queue_pending(&mut self, change: &Change) -> Result<(), AdminError> {
        let base_revision = self.revision.load(std::sync::atomic::Ordering::Acquire);
        self.store
            .queue_change(
                change.kind(),
                change.id(),
                &change.value(),
                expires_at(change.expires_at),
                base_revision,
            )
            .map_err(|_| AdminError::Store)
    }

    /// Writes the durable value first, then the live value. A live failure
    /// restores the previous durable value, so a caller reads `applied` only
    /// after both states hold the new value.
    fn apply_now(
        &mut self,
        change: &Change,
        actor: AuditActor,
    ) -> Result<(), AdminError> {
        let previous = self
            .store
            .apply_change(change.kind(), change.id(), &change.value())
            .map_err(|_| AdminError::Store)?;
        self.finish_live(change, previous.as_deref(), actor)
    }

    /// Applies `change` live once its durable value is written. Success
    /// records the new admin revision with the audit record; a live failure
    /// restores `previous`, the durable value that the change replaced.
    fn finish_live(
        &mut self,
        change: &Change,
        previous: Option<&str>,
        actor: AuditActor,
    ) -> Result<(), AdminError> {
        match self.apply_live(change) {
            Ok(revision) => self
                .store
                .record_applied_change(actor, revision)
                .map_err(|_| AdminError::Store),
            Err(error) => {
                self.store
                    .revert_change(change.kind(), change.id(), previous, actor)
                    .map_err(|_| AdminError::Store)?;
                Err(error)
            }
        }
    }

    /// Applies `change` live and returns the admin revision it moved to.
    fn apply_live(&self, change: &Change) -> Result<u64, AdminError> {
        self.apply_live_inner(change)?;
        Ok(self
            .revision
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            .wrapping_add(1))
    }

    fn apply_live_inner(&self, change: &Change) -> Result<(), AdminError> {
        let live = self.live.as_ref();
        match &change.request {
            ChangeRequest::Limit { id, limit } => {
                live.map_or(Ok(()), |live| live.set_queue_limit(*id, *limit))
            }
            ChangeRequest::Workspace { id, path } => {
                live.map_or(Ok(()), |live| live.apply_workspace(id, path))?;
                // Each workspace id keeps its own path, and one of them is
                // live; a restart makes that one live again.
                self.store.set_current_workspace(path).map_err(|_| AdminError::Store)
            }
            ChangeRequest::Grant { id, grant } => {
                let text = crate::identity::encode_grant(grant)
                    .map_err(|_| AdminError::InvalidField("grant"))?;
                live.map_or(Ok(()), |live| live.apply_grant(id, &text))
            }
            // Renews the node's already-configured, already-pinned directory
            // record. Not "switch to a new directory": that would need a new
            // certificate to pin trust to, which this change type carries no
            // field for. The live node itself rejects a `url` that does not
            // match the configured one.
            ChangeRequest::Directory { url, .. } => live
                .ok_or(AdminError::Unsupported("directory"))
                .and_then(|live| live.renew_directory(url)),
            // Publishes the pinned directory record with this capability
            // replacing the current one — see
            // `LiveNode::publish_capability`'s own doc comment for why that is
            // this change type's only concrete meaning today.
            ChangeRequest::Capability { capability, .. } => live
                .ok_or(AdminError::Unsupported("capability"))
                .and_then(|live| live.publish_capability(capability)),
        }
    }

    fn change_requires_confirmation(&self, change: &Change, actor: AuditActor) -> bool {
        if actor == AuditActor::Ui {
            return false;
        }
        match &change.request {
            ChangeRequest::Limit { id, limit } => {
                !self.live.as_ref().is_some_and(|live| {
                    live.queue_limit(*id).is_ok_and(|current| *limit <= current)
                })
            }
            _ => change.is_high_risk(),
        }
    }
    /// Confirms a durably pending change the same way `apply_now` applies an
    /// immediate one: write the durable value, apply it live, and revert the
    /// durable value if the live application fails.
    pub fn confirm_ui(&mut self, id: &str) -> Result<(), AdminError> {
        let result = self.confirm_pending(id);
        self.audited(result)
    }

    fn confirm_pending(&mut self, id: &str) -> Result<(), AdminError> {
        let pending = self.store.pending_change_ignoring_expiry(id).map_err(
            |error| match error {
                StoreError::NotFound => AdminError::ConfirmationRequired,
                _ => AdminError::Store,
            },
        )?;
        let change = change_from_pending(&pending)?;
        self.check_not_stale(id)?;
        let previous =
            self.store.confirm_pending_change(id).map_err(|error| match error {
                StoreError::InvalidState => AdminError::Expired,
                StoreError::NotFound => AdminError::ConfirmationRequired,
                _ => AdminError::Store,
            })?;
        self.finish_live(&change, previous.as_deref(), AuditActor::Ui)
    }

    /// Rejects confirming `id` if some other change was applied live after
    /// it was queued. A change that an older node queued has no revision and
    /// is not checked.
    fn check_not_stale(&self, id: &str) -> Result<(), AdminError> {
        let base = self.store.pending_revision(id).map_err(|_| AdminError::Store)?;
        match base {
            Some(base)
                if base != self.revision.load(std::sync::atomic::Ordering::Acquire) =>
            {
                Err(AdminError::Stale)
            }
            _ => Ok(()),
        }
    }

    #[must_use]
    pub fn current_workspace(&self) -> Option<String> {
        self.live.as_ref().and_then(|live| live.workspace())
    }

    pub fn grants(&self) -> Result<Vec<aether_core::Grant>, AdminError> {
        match self.live.as_ref() {
            Some(live) => live.grants(),
            None => Ok(Vec::new()),
        }
    }

    /// Revokes grant `id` for the local operator. The stored copy goes, a
    /// revocation mark stays so a grant from the policy file is not
    /// restored at the next start, and the running node drops the grant at
    /// once.
    pub fn revoke(&mut self, id: &str, actor: AuditActor) -> Result<(), AdminError> {
        let result = self.revoke_inner(id, actor);
        self.audited(result)
    }

    fn revoke_inner(&mut self, id: &str, actor: AuditActor) -> Result<(), AdminError> {
        valid(id, "id")?;
        let live = self.live.clone().ok_or(AdminError::Unsupported("revoke"))?;
        self.store.revoke_applied_grant(id).map_err(|_| AdminError::Store)?;
        live.revoke_grant(id)?;
        self.store
            .record_change(actor, AuditResult::Applied)
            .map_err(|_| AdminError::Store)
    }

    pub fn save_policy_settings(
        &mut self,
        settings: &crate::policy::PolicySettings,
        actor: AuditActor,
    ) -> Result<(), AdminError> {
        crate::policy::save_policy_settings_in_store(&self.store, settings).map_err(
            |error| match error {
                crate::policy::PolicyError::InvalidField(_) => {
                    AdminError::InvalidField("policy")
                }
                _ => AdminError::Store,
            },
        )?;
        self.store
            .record_change(actor, AuditResult::Applied)
            .map_err(|_| AdminError::Store)
    }

    pub fn set_relay(&self, relay: Option<String>) -> Result<(), AdminError> {
        self.live.as_ref().ok_or(AdminError::Unsupported("relay"))?.set_relay(relay)
    }

    /// Starts a round of the node's outbox at once, so that a new task does
    /// not wait for the next tick. A process without an outbox answers
    /// [`AdminError::Unsupported`].
    pub fn wake_outbox(&self) -> Result<(), AdminError> {
        self.live.as_ref().ok_or(AdminError::Unsupported("outbox"))?.wake_outbox()
    }

    pub fn cancel_task(&self, task_id: &str) -> Result<(), AdminError> {
        valid(task_id, "task_id")?;
        self.live.as_ref().ok_or(AdminError::Task)?.cancel_task(task_id)
    }

    #[must_use]
    pub fn store_access(&self) -> AdminStoreAccess {
        if self.store.audit_records_page(0, 1).is_ok() {
            AdminStoreAccess::Ready
        } else {
            AdminStoreAccess::Unavailable
        }
    }

    #[must_use]
    pub fn audit(&self) -> Vec<String> {
        self.audit_entries().into_iter().map(|entry| entry.result).collect()
    }
    #[must_use]
    pub fn audit_entries(&self) -> Vec<AuditEntry> {
        self.store.audit_records().map_or_else(
            |_| Vec::new(),
            |records| {
                records
                    .into_iter()
                    .map(|record| AuditEntry {
                        actor: record.actor.as_str().to_owned(),
                        action: record.action.as_str().to_owned(),
                        result: record.result.as_str().to_owned(),
                    })
                    .collect()
            },
        )
    }
    #[must_use]
    pub fn pending(&self) -> Vec<PendingChange> {
        self.store.pending_changes().map_or_else(
            |_| Vec::new(),
            |changes| {
                changes
                    .into_iter()
                    .map(|change| PendingChange {
                        id: change.id,
                        kind: change.kind.as_str().to_owned(),
                    })
                    .collect()
            },
        )
    }

    #[must_use]
    pub fn configured_change_counts(&self) -> BTreeMap<ChangeKind, usize> {
        self.store.applied_change_counts().unwrap_or_default()
    }

    #[must_use]
    pub fn node_summary(&self) -> NodeSummary {
        self.store.summary().unwrap_or_default()
    }

    /// Each caller endpoint's newest task in `scope_id`.
    #[must_use]
    pub fn last_tasks(&self, scope_id: &str) -> Vec<aether_store::LastTask> {
        self.store.last_tasks(scope_id).unwrap_or_default()
    }

    /// At most `limit` audit results, after the first `skip`.
    #[must_use]
    pub fn audit_page(&self, skip: usize, limit: usize) -> Vec<String> {
        self.store.audit_records_page(skip, limit).map_or_else(
            |_| Vec::new(),
            |records| {
                records
                    .into_iter()
                    .map(|record| record.result.as_str().to_owned())
                    .collect()
            },
        )
    }

    #[must_use]
    pub fn task_activity_after(&self, cursor: i64, limit: usize) -> Vec<TaskActivity> {
        self.store.task_activity_after(cursor, limit).unwrap_or_default()
    }

    #[must_use]
    pub fn managed_members(&self) -> Vec<ManagedMember> {
        self.store.all_managed_members().unwrap_or_default()
    }
}

impl Change {
    #[must_use]
    pub fn is_high_risk(&self) -> bool {
        match &self.request {
            ChangeRequest::Grant { grant, .. } => {
                !crate::direct::grant_is_low_risk_invite(grant)
            }
            ChangeRequest::Workspace { .. } | ChangeRequest::Capability { .. } => true,
            _ => false,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        self.request.id()
    }

    #[must_use]
    pub const fn kind(&self) -> ChangeKind {
        self.request.kind()
    }

    fn value(&self) -> String {
        self.request.value()
    }
}

fn expires_at(value: SystemTime) -> i64 {
    aether_core::time::unix_timestamp(value).unwrap_or(i64::MAX)
}

/// Rebuilds a typed `Change` from a durably stored pending change so
/// `confirm_ui` can apply it live the same way an immediate change is
/// applied. `expires_at` is not meaningful here: the store checks the
/// expiry when it confirms the pending row.
fn change_from_pending(
    pending: &aether_store::PendingChange,
) -> Result<Change, AdminError> {
    let request = ChangeRequest::from_stored(
        pending.kind,
        pending.id.clone(),
        pending.value.clone(),
    )?;
    Ok(Change { request, expires_at: SystemTime::now() })
}
#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
pub(crate) fn peer_grant() -> aether_core::Grant {
    use crate::test_support::{signed_test_grant, test_signing_key};
    use aether_core::{AgentId, GrantId};
    signed_test_grant(&test_signing_key(), |input| {
        input.id = GrantId::new("peer");
        input.subject_agent_id = AgentId::new("caller");
        input.nonce = [8; 32];
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AdminError, Administration, ChangeOutcome, ChangeRequest, PendingChange,
        peer_grant, shared_with_store, test_support::FakeNode,
    };
    use aether_store::NodeStore;
    use std::sync::Arc;
    #[test]
    fn mcp_cannot_confirm_high_risk_change() {
        let (_directory, mut admin) = in_temp_dir();
        let change =
            ChangeRequest::Workspace { id: "one".to_owned(), path: "safe".to_owned() }
                .into_change()
                .expect("change must parse");
        assert_eq!(
            admin.submit(&change, aether_store::AuditActor::Mcp, false),
            Ok(ChangeOutcome::Pending)
        );
        admin.confirm_ui("one").expect("ui confirmation");
        assert_eq!(admin.audit(), ["pending", "confirmed", "applied"]);
    }

    #[test]
    fn caller_cannot_lower_workspace_change_risk() {
        let (_directory, mut admin) = in_temp_dir();
        assert_eq!(
            admin.submit(
                &ChangeRequest::Workspace {
                    id: "workspace".to_owned(),
                    path: "project".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            ),
            Ok(ChangeOutcome::Pending)
        );
    }

    #[test]
    fn typed_changes_validate_their_own_fields() {
        assert!(matches!(
            ChangeRequest::Directory {
                id: "directory".to_owned(),
                url: "http://directory.test".to_owned(),
            }
            .into_change(),
            Err(AdminError::InvalidField("url"))
        ));
        assert!(matches!(
            ChangeRequest::Limit {
                id: aether_core::limits::QueueLimitId::Global,
                limit: 0,
            }
            .into_change(),
            Err(AdminError::InvalidField("limit"))
        ));
    }

    #[test]
    fn task_cancellation_uses_the_live_node_callback() {
        let (_directory, mut administration) = in_temp_dir();
        let node = Arc::new(FakeNode::default());
        administration.set_live(node.clone());
        administration
            .cancel_task("00000000-0000-4000-8000-000000000001")
            .expect("task must cancel");
        assert_eq!(node.calls(), ["cancel 00000000-0000-4000-8000-000000000001"]);
    }

    #[test]
    fn queue_limit_decrease_applies_without_confirmation() {
        let (_directory, mut administration) = in_temp_dir();
        let node = Arc::new(FakeNode::default().with_queue_limit(4));
        administration.set_live(node.clone());
        administration
            .submit(
                &limit_change(aether_core::limits::QueueLimitId::Global, 2),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("limit decrease must apply");
        assert_eq!(node.calls(), ["limit queue_global 2"]);
        assert!(administration.pending().is_empty());
    }

    #[test]
    fn queue_limit_increase_requires_local_confirmation() {
        let (_directory, mut administration) = in_temp_dir();
        administration.set_live(Arc::new(FakeNode::default().with_queue_limit(1)));
        administration
            .submit(
                &limit_change(aether_core::limits::QueueLimitId::Global, 2),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("limit increase must queue");
        assert_eq!(
            administration.pending(),
            [PendingChange { id: "queue_global".to_owned(), kind: "limit".to_owned() }]
        );
    }

    #[test]
    fn a_missing_or_failed_limit_read_requires_confirmation() {
        // No reader at all: nothing proves this is not an increase, so it
        // must queue rather than apply silently.
        let (_directory, mut administration) = in_temp_dir();
        administration
            .submit(
                &limit_change(aether_core::limits::QueueLimitId::Global, 2),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("limit change without a reader must queue");
        assert_eq!(
            administration.pending(),
            [PendingChange { id: "queue_global".to_owned(), kind: "limit".to_owned() }]
        );

        // A reader that errors on every call is exactly as unproven as no
        // reader at all.
        let (_directory, mut administration) = in_temp_dir();
        administration.set_live(Arc::new(FakeNode::default()));
        administration
            .submit(
                &limit_change(aether_core::limits::QueueLimitId::PerPeer, 2),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("limit change with a failing reader must queue");
        assert_eq!(
            administration.pending(),
            [PendingChange {
                id: "queue_per_peer".to_owned(),
                kind: "limit".to_owned()
            }]
        );
    }

    #[test]
    fn queue_limit_change_uses_the_live_node_callback() {
        let (_directory, mut administration) = in_temp_dir();
        let node = Arc::new(FakeNode::default().with_queue_limit(5));
        administration.set_live(node.clone());
        administration
            .submit(
                &ChangeRequest::Limit {
                    id: aether_core::limits::QueueLimitId::Global,
                    limit: 2,
                }
                .into_change()
                .expect("limit change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("limit change must apply");
        assert_eq!(node.calls(), ["limit queue_global 2"]);
    }

    #[test]
    fn confirmed_change_updates_durable_node_state() {
        let directory = tempfile::tempdir().expect("directory must create");
        let database = directory.path().join(aether_store::NODE_DATABASE);
        let store = NodeStore::open(&database).expect("store must open");
        let node = Arc::new(FakeNode::default());
        let mut administration = Administration::with_store(store);
        administration.set_live(node.clone());
        administration
            .submit(
                &ChangeRequest::Workspace {
                    id: "workspace".to_owned(),
                    path: "project".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("change must queue");
        // Queuing a high-risk change must not touch the live node yet.
        assert!(node.calls().is_empty());
        administration.confirm_ui("workspace").expect("change must confirm");
        // UI confirmation must apply the change to the running node, not
        // only record it durably.
        assert_eq!(node.calls(), ["workspace workspace project"]);
        drop(administration);
        let store = NodeStore::open(&database).expect("store must reopen");
        assert_eq!(
            store.applied_change(aether_store::ChangeKind::Workspace, "workspace"),
            Ok(Some("project".to_owned()))
        );
    }

    #[test]
    fn a_stale_confirmation_is_rejected_with_a_store_backing_the_pending_change() {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::in_dir(directory.path()).expect("store must open");
        let mut administration = Administration::with_store(store);
        administration.set_live(Arc::new(
            FakeNode::default().with_directory("https://directory.test"),
        ));
        administration
            .submit(
                &ChangeRequest::Workspace {
                    id: "workspace".to_owned(),
                    path: "project".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("high-risk change must queue");
        administration
            .submit(
                &ChangeRequest::Directory {
                    id: "directory".to_owned(),
                    url: "https://directory.test".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("unrelated low-risk change must apply immediately");
        assert_eq!(administration.confirm_ui("workspace"), Err(AdminError::Stale));
    }

    #[test]
    fn a_stale_confirmation_is_detected_after_a_restart() {
        let directory = tempfile::tempdir().expect("directory must create");
        let database = directory.path().join(aether_store::NODE_DATABASE);
        let mut administration = Administration::with_store(
            NodeStore::open(&database).expect("store must open"),
        );
        administration.set_live(Arc::new(
            FakeNode::default().with_directory("https://directory.test"),
        ));
        administration
            .submit(
                &ChangeRequest::Workspace {
                    id: "workspace".to_owned(),
                    path: "project".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("high-risk change must queue");
        administration
            .submit(
                &ChangeRequest::Directory {
                    id: "directory".to_owned(),
                    url: "https://directory.test".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("unrelated low-risk change must apply immediately");
        // The "restart": a fresh `Administration` over the same durable
        // database, so only the durable `admin.revision` and
        // `admin.pending_revision.workspace` keys can catch this.
        drop(administration);
        let mut restarted = Administration::with_store(
            NodeStore::open(&database).expect("store must reopen"),
        );
        assert_eq!(restarted.confirm_ui("workspace"), Err(AdminError::Stale));
    }

    #[test]
    fn a_failed_confirmation_reverts_the_durable_value() {
        let directory = tempfile::tempdir().expect("directory must create");
        let database = directory.path().join(aether_store::NODE_DATABASE);
        let store = NodeStore::open(&database).expect("store must open");
        let mut administration = Administration::with_store(store);
        let node = Arc::new(FakeNode::default());
        node.fail_with(AdminError::InvalidField("grant"));
        administration.set_live(node);
        let grant = peer_grant();
        administration
            .submit(
                &ChangeRequest::Grant { id: grant.id.as_str().to_owned(), grant }
                    .into_change()
                    .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("change must queue");
        assert_eq!(
            administration.confirm_ui("peer"),
            Err(AdminError::InvalidField("grant"))
        );
        // The failed live application must leave no durable trace and the
        // pending change gone, matching how an immediate change behaves.
        assert!(administration.pending().is_empty());
        drop(administration);
        let store = NodeStore::open(&database).expect("store must reopen");
        assert_eq!(
            store.applied_change(aether_store::ChangeKind::Grant, "peer"),
            Ok(None)
        );
    }

    #[test]
    fn a_storage_failure_during_confirmation_leaves_the_live_node_unchanged() {
        let directory = tempfile::tempdir().expect("directory must create");
        let database = directory.path().join(aether_store::NODE_DATABASE);
        let store = NodeStore::open(&database).expect("store must open");
        let mut administration = Administration::with_store(store);
        let node = Arc::new(FakeNode::default());
        administration.set_live(node.clone());
        administration
            .submit(
                &ChangeRequest::Workspace {
                    id: "workspace".to_owned(),
                    path: "project".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("change must queue");
        // Another connection holds the write lock past the busy timeout, so
        // the confirming write really fails: a storage failure, not a
        // logical one (missing, expired, or stale).
        let writer = rusqlite::Connection::open(&database).expect("writer must open");
        writer.execute_batch("BEGIN IMMEDIATE").expect("writer must lock");
        let result = administration.confirm_ui("workspace");
        writer.execute_batch("ROLLBACK").expect("writer must unlock");
        assert_eq!(result, Err(AdminError::Store));
        assert!(node.calls().is_empty());
        assert_eq!(
            administration
                .pending()
                .into_iter()
                .map(|change| change.id)
                .collect::<Vec<_>>(),
            ["workspace"]
        );
    }

    #[test]
    #[expect(
        clippy::needless_collect,
        clippy::significant_drop_tightening,
        reason = "all threads must start before the first join, so they run at \
                  once; the set-up lock has no other user"
    )]
    fn concurrent_workspace_changes_apply_or_go_stale_without_corrupting_state() {
        let directory = tempfile::tempdir().expect("directory must create");
        let database = directory.path().join(aether_store::NODE_DATABASE);
        let store = NodeStore::open(&database).expect("store must open");
        let administration = shared_with_store(store);
        let node = Arc::new(FakeNode::default());
        administration.lock().expect("administration must lock").set_live(node.clone());
        // Real OS threads, all sharing the one `Arc<Mutex<Administration>>`
        // the socket, MCP, and UI all actually share in production — not a
        // proof that `Mutex` excludes races (it always does), but a proof
        // that a real concurrent workload against the real SQLite-backed
        // store does not deadlock, panic, or leave a change half-applied.
        // Each thread queues, then immediately tries to confirm, its own
        // distinctly-id'd workspace change; because every successful
        // confirmation advances the shared revision, a thread whose
        // confirmation lands after another thread's is expected —not
        // corrected for— to see `AdminError::Stale`, exactly as a
        // sequential caller racing a concurrent one should.
        let threads = (0..8)
            .map(|index| {
                let administration = Arc::clone(&administration);
                std::thread::spawn(move || {
                    let id = format!("workspace-{index}");
                    let path = format!("project-{index}");
                    administration
                        .lock()
                        .expect("administration must lock")
                        .submit(
                            &ChangeRequest::Workspace {
                                id: id.clone(),
                                path: path.clone(),
                            }
                            .into_change()
                            .expect("change must parse"),
                            aether_store::AuditActor::Mcp,
                            false,
                        )
                        .expect("change must queue");
                    let result = administration
                        .lock()
                        .expect("administration must lock")
                        .confirm_ui(&id);
                    (id, path, result)
                })
            })
            .collect::<Vec<_>>();
        let outcomes = threads
            .into_iter()
            .map(|thread| thread.join().expect("thread must not panic"))
            .collect::<Vec<_>>();
        for (id, path, result) in &outcomes {
            assert!(
                matches!(result, Ok(()) | Err(AdminError::Stale)),
                "{id} confirmed with an unexpected result: {result:?}"
            );
            let seen_live = node.calls().contains(&format!("workspace {id} {path}"));
            // The two must never disagree: a reported success without the
            // applier having actually run (or vice versa) would mean the
            // durable-then-live sequence tore under concurrent access.
            assert_eq!(
                result.is_ok(),
                seen_live,
                "{id} disagrees on whether it applied"
            );
        }
        let confirmed = outcomes.iter().filter(|(.., result)| result.is_ok()).count();
        assert!(confirmed >= 1, "at least the first confirmation must succeed");
        let administration = administration.lock().expect("administration must lock");
        for (id, path, result) in &outcomes {
            if result.is_ok() {
                assert_eq!(
                    administration
                        .store
                        .applied_change(aether_store::ChangeKind::Workspace, id)
                        .ok()
                        .flatten(),
                    Some(path.clone())
                );
            } else {
                assert!(
                    administration.pending().iter().any(|change| &change.id == id),
                    "{id} was rejected as stale but is no longer queued"
                );
            }
        }
    }

    #[test]
    fn a_directory_change_reports_itself_unsupported_without_an_applier() {
        let (_directory, mut administration) = in_temp_dir();
        assert_eq!(
            administration.submit(
                &ChangeRequest::Directory {
                    id: "directory".to_owned(),
                    url: "https://directory.test".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            ),
            Err(AdminError::Unsupported("directory"))
        );
    }

    #[test]
    fn a_directory_change_uses_the_live_applier_and_rejects_the_wrong_url() {
        let (_directory, mut administration) = in_temp_dir();
        let node =
            Arc::new(FakeNode::default().with_directory("https://directory.test"));
        administration.set_live(node.clone());
        assert_eq!(
            administration.submit(
                &ChangeRequest::Directory {
                    id: "directory".to_owned(),
                    url: "https://other.test".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            ),
            Err(AdminError::InvalidField("url"))
        );
        assert!(node.calls().is_empty());
        administration
            .submit(
                &ChangeRequest::Directory {
                    id: "directory".to_owned(),
                    url: "https://directory.test".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("matching url must apply");
        assert_eq!(node.calls(), ["directory https://directory.test"]);
    }

    #[test]
    fn a_capability_change_reports_itself_unsupported_without_an_applier() {
        let (_directory, mut administration) = in_temp_dir();
        administration
            .submit(
                &ChangeRequest::Capability {
                    id: "capability".to_owned(),
                    capability: "review".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("high-risk change must queue");
        assert_eq!(
            administration.confirm_ui("capability"),
            Err(AdminError::Unsupported("capability"))
        );
    }

    #[test]
    fn a_capability_change_uses_the_live_applier() {
        let (_directory, mut administration) = in_temp_dir();
        let node = Arc::new(FakeNode::default());
        administration.set_live(node.clone());
        administration
            .submit(
                &ChangeRequest::Capability {
                    id: "capability".to_owned(),
                    capability: "review".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("high-risk change must queue");
        assert!(node.calls().is_empty());
        administration.confirm_ui("capability").expect("change must confirm");
        assert_eq!(node.calls(), ["capability review"]);
    }

    #[test]
    fn an_expired_pending_change_cannot_confirm() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut store = NodeStore::in_dir(directory.path()).expect("store must open");
        store
            .queue_change(
                aether_store::ChangeKind::Workspace,
                "workspace",
                "project",
                1,
                0,
            )
            .expect("change must queue");
        let mut administration = Administration::with_store(store);
        assert_eq!(administration.confirm_ui("workspace"), Err(AdminError::Expired));
        assert_eq!(
            administration.confirm_ui("unknown"),
            Err(AdminError::ConfirmationRequired)
        );
    }

    #[test]
    fn a_failed_live_change_restores_the_durable_value() {
        let directory = tempfile::tempdir().expect("directory must create");
        let database = directory.path().join(aether_store::NODE_DATABASE);
        let store = NodeStore::open(&database).expect("store must open");
        let node = Arc::new(FakeNode::default().with_queue_limit(10));
        let mut administration = Administration::with_store(store);
        administration.set_live(node.clone());
        administration
            .submit(
                &limit_change(aether_core::limits::QueueLimitId::Global, 2),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("first limit must apply");
        node.fail_with(AdminError::InvalidField("limit"));
        assert_eq!(
            administration.submit(
                &limit_change(aether_core::limits::QueueLimitId::Global, 5),
                aether_store::AuditActor::Mcp,
                false
            ),
            Err(AdminError::InvalidField("limit"))
        );
        assert_eq!(administration.audit(), ["applied".to_owned(), "failed".to_owned()]);
        drop(administration);

        let store = NodeStore::open(&database).expect("store must reopen");
        assert_eq!(
            store.applied_change(aether_store::ChangeKind::Limit, "queue_global"),
            Ok(Some("2".to_owned()))
        );
    }

    #[test]
    fn a_failed_first_live_change_leaves_no_durable_value() {
        let directory = tempfile::tempdir().expect("directory must create");
        let database = directory.path().join(aether_store::NODE_DATABASE);
        let store = NodeStore::open(&database).expect("store must open");
        let mut administration = Administration::with_store(store);
        let node = Arc::new(FakeNode::default().with_queue_limit(10));
        node.fail_with(AdminError::InvalidField("limit"));
        administration.set_live(node);
        assert_eq!(
            administration.submit(
                &limit_change(aether_core::limits::QueueLimitId::PerPeer, 4),
                aether_store::AuditActor::Mcp,
                false
            ),
            Err(AdminError::InvalidField("limit"))
        );
        drop(administration);

        let store = NodeStore::open(&database).expect("store must reopen");
        assert_eq!(
            store.applied_change(aether_store::ChangeKind::Limit, "queue_per_peer"),
            Ok(None)
        );
    }

    /// An administration over a node store in a new temporary directory.
    fn in_temp_dir() -> (tempfile::TempDir, Administration) {
        let directory = tempfile::tempdir().expect("directory must create");
        let store = NodeStore::in_dir(directory.path()).expect("store must open");
        (directory, Administration::with_store(store))
    }

    fn limit_change(
        id: aether_core::limits::QueueLimitId,
        limit: u32,
    ) -> crate::admin::Change {
        ChangeRequest::Limit { id, limit }
            .into_change()
            .expect("limit change must parse")
    }

    #[test]
    fn pending_typed_change_survives_a_restart() {
        let directory = tempfile::tempdir().expect("directory must create");
        let database = directory.path().join(aether_store::NODE_DATABASE);
        let store = NodeStore::open(&database).expect("store must open");
        let mut administration = Administration::with_store(store);
        administration
            .submit(
                &ChangeRequest::Workspace {
                    id: "workspace".to_owned(),
                    path: "project".to_owned(),
                }
                .into_change()
                .expect("change must parse"),
                aether_store::AuditActor::Mcp,
                false,
            )
            .expect("change must queue");
        drop(administration);

        let store = NodeStore::open(&database).expect("store must reopen");
        let mut administration = Administration::with_store(store);
        assert_eq!(
            administration.pending(),
            [PendingChange {
                id: "workspace".to_owned(),
                kind: "workspace".to_owned()
            }]
        );
        administration.confirm_ui("workspace").expect("change must confirm");
        drop(administration);

        let store = NodeStore::open(&database).expect("store must reopen");
        assert_eq!(
            store.applied_change(aether_store::ChangeKind::Workspace, "workspace"),
            Ok(Some("project".to_owned()))
        );
    }
}
