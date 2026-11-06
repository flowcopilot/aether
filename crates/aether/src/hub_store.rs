use std::{collections::BTreeSet, path::Path};

use aether_store::{
    AccessRequestDraft, ApprovalSide, HubApproval, NodeStore, StoreError,
};

pub struct HubStore(NodeStore);

impl HubStore {
    pub fn open(data_dir: &Path) -> Result<Self, StoreError> {
        NodeStore::in_dir(data_dir).map(Self)
    }

    pub fn approval(&self, invite_id: &str) -> Result<Option<HubApproval>, StoreError> {
        self.0.hub_approval(invite_id)
    }

    pub fn is_blocked(&self, endpoint: &str) -> Result<bool, StoreError> {
        self.0.hub_blocked(endpoint)
    }

    pub fn put_approval(&self, approval: &HubApproval) -> Result<(), StoreError> {
        self.0.put_hub_approval(approval)
    }

    pub fn remove_approval(&self, invite_id: &str) -> Result<(), StoreError> {
        self.0.remove_hub_approval(invite_id)
    }

    pub fn put_block(
        &self,
        endpoint: &str,
        peer_name: &str,
        at: i64,
    ) -> Result<(), StoreError> {
        self.0.put_hub_block(endpoint, peer_name, at)
    }

    pub fn remove_block(&self, endpoint: &str) -> Result<(), StoreError> {
        self.0.remove_hub_block(endpoint)
    }

    pub fn pin_approval(
        &self,
        approval: &HubApproval,
        endpoint: &str,
        bundle: &str,
    ) -> Result<(), StoreError> {
        self.0.pin_hub_approval(approval, endpoint, bundle)
    }

    pub fn clear_expired_approvals(&self, now: i64) -> Result<(), StoreError> {
        self.0.clear_expired_hub_approvals(now).map(|_| ())
    }

    pub fn access_request_draft(
        &self,
        id: &str,
    ) -> Result<Option<AccessRequestDraft>, StoreError> {
        self.0.access_request_draft(id)
    }

    pub fn remove_access_request_draft(&self, id: &str) -> Result<(), StoreError> {
        self.0.remove_access_request_draft(id)
    }

    pub fn sender_approved_invite_ids(
        &self,
        invite_ids: impl IntoIterator<Item = String>,
    ) -> Result<BTreeSet<String>, StoreError> {
        let mut approved = BTreeSet::new();
        for invite_id in invite_ids {
            if self
                .approval(&invite_id)?
                .is_some_and(|approval| approval.side == ApprovalSide::Sender)
            {
                approved.insert(invite_id);
            }
        }
        Ok(approved)
    }
}
