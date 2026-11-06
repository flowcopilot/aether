use std::sync::atomic::Ordering;

use aether_core::{
    Admission, Denial, EndpointId, Request, Scope, ScopeKind, TrustProvider,
};
use aether_store::{TaskAdmission as StoreAdmission, TaskInput};

use super::{Identity, ProofError, ProofNode, SubmissionInput, request_fingerprint};
use crate::execution::{Agent, Task};

impl<T, P> ProofNode<T, P>
where
    T: TrustProvider + 'static,
    P: Agent + Clone + Send + 'static,
{
    pub fn admit(
        &self,
        input: SubmissionInput<'_>,
        identity: Identity<'_>,
        replay_key: Option<String>,
    ) -> Result<StoreAdmission, ProofError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(ProofError::Shutdown);
        }
        let SubmissionInput { request, authenticated_endpoint, idempotency_key, task } =
            input;
        if task.id != request.task_id.as_str()
            || task.workspace_id != request.workspace_id.as_str()
        {
            return Err(ProofError::TaskMismatch);
        }
        let scope = self
            .scopes
            .get(&request.scope_id)
            .ok_or(ProofError::Denied(Denial::ScopeMismatch))?;
        let policy = self.policy.lock().map_err(|_| ProofError::Internal)?;
        let admission = match identity {
            Identity::Claim(claim) => {
                policy.admit(request, claim, scope, authenticated_endpoint)
            }
            Identity::Verified => {
                policy.admit_verified(request, scope, authenticated_endpoint)
            }
        };
        drop(policy);
        if let Admission::Denied(denial) = admission {
            return Err(ProofError::Denied(denial));
        }
        self.check_group(scope, request, authenticated_endpoint)?;
        self.enqueue(request, authenticated_endpoint, idempotency_key, task, replay_key)
    }

    fn check_group(
        &self,
        scope: &Scope,
        request: &Request,
        endpoint: &EndpointId,
    ) -> Result<(), ProofError> {
        if scope.kind == ScopeKind::Direct {
            let agent = request.caller_agent_id.as_str().to_owned();
            let endpoint = endpoint.as_str().to_owned();
            if !self
                .store
                .call(move |store| store.access_group_allows(&agent, &endpoint))?
            {
                return Err(ProofError::GroupDenied);
            }
        }
        Ok(())
    }

    fn enqueue(
        &self,
        request: &Request,
        authenticated_endpoint: &EndpointId,
        idempotency_key: &str,
        task: &Task,
        replay_key: Option<String>,
    ) -> Result<StoreAdmission, ProofError> {
        let input = TaskInput {
            request: Request {
                caller_endpoint_id: authenticated_endpoint.clone(),
                ..request.clone()
            },
            idempotency_key: idempotency_key.to_owned(),
            request_fingerprint: request_fingerprint(request, task),
            skill: task.skill.clone(),
            remote_text: task.remote_text.clone(),
        };
        let limits = self.queue_limits()?;
        let admission = {
            let _dispatch_guard =
                self.dispatch_serial.lock().map_err(|_| ProofError::Internal)?;
            self.store.admit_only(input, limits, replay_key)?
        };
        if let StoreAdmission::Accepted(task_id) = &admission {
            let _ = self.scheduler_wake.try_send(());
            let _ = self.event_sender.send(task_id.clone());
        }
        Ok(admission)
    }
}
