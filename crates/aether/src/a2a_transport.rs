use aether_core::{EndpointId, ScopeKind, TrustProvider};

use aether_store::StoreError;

use crate::{
    execution::{Agent, Task},
    proof::{Identity, ProofError, ProofNode, SubmissionInput},
    transport::{
        A2aCancel, A2aPartContent, A2aRequest, A2aTaskStatus, Frame, FrameKind,
        MAX_EXCHANGE_BYTES, MAX_FRAMES, TransportError, event_payload,
        text_result_payload, text_result_payload_with_progress,
    },
};

const EVENT_PAGE_BYTE_BUDGET: usize = MAX_EXCHANGE_BYTES - 65_536;

pub struct A2aTransport<'a, T, P> {
    node: &'a ProofNode<T, P>,
}

impl<'a, T, P> A2aTransport<'a, T, P> {
    #[must_use]
    pub const fn new(node: &'a ProofNode<T, P>) -> Self {
        Self { node }
    }

    pub fn handle(
        &self,
        authenticated_endpoint: &str,
        frames: Vec<Frame>,
    ) -> Result<Vec<Frame>, TransportError>
    where
        T: TrustProvider + 'static,
        P: Agent + Clone + Send + 'static,
    {
        let Ok([mut frame]) = <[Frame; 1]>::try_from(frames) else {
            return Err(TransportError::InvalidOrder);
        };
        if frame.kind == FrameKind::Cancel {
            return self.cancel(authenticated_endpoint, frame);
        }
        if frame.kind != FrameKind::Request {
            return Err(TransportError::BadFrame);
        }
        let payload: A2aRequest = serde_json::from_value(frame.payload.take())
            .map_err(|_| TransportError::BadFrame)?;
        let (request, claim) = payload.policy_data(&frame.scope_id)?;
        let endpoint = EndpointId::new(authenticated_endpoint);
        if request.caller_endpoint_id != endpoint {
            return Err(TransportError::EndpointMismatch);
        }
        let text = match payload.task.history.as_deref() {
            Some([message]) if message.role == a2a::Role::User => {
                match message.parts.as_slice() {
                    [part] => match &part.content {
                        A2aPartContent::Text(text) => text,
                        _ => return Err(TransportError::BadFrame),
                    },
                    _ => return Err(TransportError::BadFrame),
                }
            }
            _ => return Err(TransportError::BadFrame),
        };
        let task = Task {
            id: payload.task.id,
            skill: task_skill(
                self.node.scope_kind_for(&frame.scope_id),
                request.capability_id.as_str(),
            ),
            workspace_id: request.workspace_id.as_str().to_owned(),
            remote_text: text.to_owned(),
        };
        let admission = self
            .node
            .admit(
                SubmissionInput {
                    request: &request,
                    authenticated_endpoint: &endpoint,
                    idempotency_key: &payload.idempotency_key,
                    task: &task,
                },
                Identity::Claim(&claim),
                Some(format!("{}:{}", frame.exchange_id, payload.request_nonce)),
            )
            .map_err(map_error)?;
        let (aether_store::TaskAdmission::Accepted(task_id)
        | aether_store::TaskAdmission::Existing(task_id)) = admission;
        let last_sequence = i64::try_from(payload.last_event_sequence.unwrap_or(0))
            .map_err(|_| TransportError::BadFrame)?;
        let event_limit = MAX_FRAMES.saturating_sub(1);
        let (state, result, events, mut more_events) = self
            .node
            .task_progress(&task_id, last_sequence, event_limit)
            .map_err(map_error)?;
        if matches!(
            state,
            aether_store::TaskState::UnknownResult
                | aether_store::TaskState::OperatorReviewed
        ) {
            return Err(TransportError::OperatorReviewRequired(
                state.as_str().to_owned(),
            ));
        }
        if state == aether_store::TaskState::Succeeded
            && self.node.content_is_expired(&task_id).map_err(map_error)?
        {
            return Err(TransportError::ResultExpired);
        }
        let status = a2a_status(state);
        let result_text = if state == aether_store::TaskState::Succeeded {
            result.unwrap_or_default()
        } else {
            String::new()
        };
        let mut response = Vec::with_capacity(events.len().saturating_add(1));
        let mut exchange_bytes = 0usize;
        let mut cursor = last_sequence;
        for event in events {
            let event_frame = Frame {
                version: 0,
                kind: FrameKind::Event,
                exchange_id: frame.exchange_id.clone(),
                scope_id: frame.scope_id.clone(),
                task_id: Some(task_id.clone()),
                payload: event_payload(
                    &task_id,
                    event_status(event.event_type, state),
                    Some(event.sequence),
                ),
            };
            let frame_bytes =
                serde_json::to_vec(&event_frame).map_or(0, |encoded| encoded.len());
            if exchange_bytes.saturating_add(frame_bytes) > EVENT_PAGE_BYTE_BUDGET {
                more_events = true;
                break;
            }
            exchange_bytes = exchange_bytes.saturating_add(frame_bytes);
            cursor = event.sequence;
            response.push(event_frame);
        }
        response.push(Frame {
            version: 0,
            kind: FrameKind::Result,
            exchange_id: frame.exchange_id.clone(),
            scope_id: frame.scope_id.clone(),
            task_id: Some(task_id.clone()),
            payload: text_result_payload_with_progress(
                &task_id,
                status,
                &result_text,
                cursor,
                more_events,
            ),
        });
        Ok(response)
    }

    fn cancel(
        &self,
        authenticated_endpoint: &str,
        mut frame: Frame,
    ) -> Result<Vec<Frame>, TransportError>
    where
        T: TrustProvider + 'static,
        P: Agent + Clone + Send + 'static,
    {
        let cancel: A2aCancel = serde_json::from_value(frame.payload.take())
            .map_err(|_| TransportError::BadFrame)?;
        let task_id = frame.task_id.as_deref().ok_or(TransportError::BadFrame)?;
        if cancel.id != task_id {
            return Err(TransportError::BadFrame);
        }
        self.node
            .cancel_from(
                task_id,
                &EndpointId::new(authenticated_endpoint),
                &frame.scope_id,
            )
            .map_err(map_error)?;
        let event = self
            .node
            .events_after(task_id, 0)
            .map_err(map_error)?
            .into_iter()
            .last()
            .ok_or_else(|| TransportError::Network("task event missing".to_owned()))?;
        Ok(vec![
            Frame {
                version: 0,
                kind: FrameKind::Event,
                exchange_id: frame.exchange_id.clone(),
                scope_id: frame.scope_id.clone(),
                task_id: frame.task_id.clone(),
                payload: event_payload(
                    task_id,
                    event_status(event.event_type, aether_store::TaskState::Cancelled),
                    Some(event.sequence),
                ),
            },
            Frame {
                version: 0,
                kind: FrameKind::Result,
                exchange_id: frame.exchange_id.clone(),
                scope_id: frame.scope_id.clone(),
                task_id: frame.task_id.clone(),
                payload: text_result_payload(task_id, A2aTaskStatus::Canceled, ""),
            },
        ])
    }
}

fn task_skill(kind: Option<ScopeKind>, capability: &str) -> String {
    if kind == Some(ScopeKind::Direct) && capability == "review" {
        crate::execution::GENERAL_SKILL
    } else {
        capability
    }
    .to_owned()
}

fn event_status(
    event: aether_store::EventType,
    current: aether_store::TaskState,
) -> A2aTaskStatus {
    a2a_status(event.task_state().unwrap_or(current))
}

const fn a2a_status(state: aether_store::TaskState) -> A2aTaskStatus {
    match state {
        aether_store::TaskState::PendingApproval => A2aTaskStatus::InputRequired,
        aether_store::TaskState::Queued | aether_store::TaskState::Leased => {
            A2aTaskStatus::Submitted
        }
        aether_store::TaskState::Running => A2aTaskStatus::Working,
        aether_store::TaskState::Succeeded => A2aTaskStatus::Completed,
        aether_store::TaskState::Failed
        | aether_store::TaskState::UnknownResult
        | aether_store::TaskState::OperatorReviewed => A2aTaskStatus::Failed,
        aether_store::TaskState::Rejected => A2aTaskStatus::Rejected,
        aether_store::TaskState::Cancelled => A2aTaskStatus::Canceled,
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "an adapter for `map_err`, which hands over the error by value"
)]
fn map_error(error: ProofError) -> TransportError {
    match error {
        ProofError::Denied(_)
        | ProofError::GroupDenied
        | ProofError::Store(StoreError::Unauthorized) => {
            TransportError::PermissionDenied
        }
        ProofError::Store(StoreError::Overloaded) => TransportError::Overloaded,
        ProofError::Store(StoreError::DuplicateConflict) => TransportError::Replay,
        ProofError::Store(_)
        | ProofError::Execution(_)
        | ProofError::TaskMismatch
        | ProofError::Queue
        | ProofError::Shutdown => TransportError::Network("task failed".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use aether_core::ScopeKind;
    use aether_store::{EventType, TaskState};

    use crate::transport::A2aTaskStatus;

    use super::{a2a_status, event_status, task_skill};

    #[test]
    fn a_pact_task_keeps_its_brand_capability() {
        assert_eq!(task_skill(Some(ScopeKind::Pact), "review"), "review");
        assert_eq!(task_skill(Some(ScopeKind::Pact), "brand-copy"), "brand-copy");
        assert_eq!(
            task_skill(Some(ScopeKind::Direct), "review"),
            crate::execution::GENERAL_SKILL
        );
    }

    #[test]
    fn event_status_uses_the_event_when_it_names_a_state() {
        for (event, status) in [
            (
                EventType::Entered(TaskState::PendingApproval),
                A2aTaskStatus::InputRequired,
            ),
            (EventType::Entered(TaskState::UnknownResult), A2aTaskStatus::Failed),
            (EventType::Entered(TaskState::OperatorReviewed), A2aTaskStatus::Failed),
            (EventType::Requeued, A2aTaskStatus::Submitted),
            (EventType::LeaseExpired, A2aTaskStatus::Submitted),
            (EventType::Progress, A2aTaskStatus::Working),
        ] {
            assert_eq!(event_status(event, TaskState::Succeeded), status);
        }
        assert_eq!(
            event_status(EventType::ContentExpired, TaskState::Rejected),
            A2aTaskStatus::Rejected
        );
    }

    #[test]
    fn a2a_status_maps_store_states() {
        assert_eq!(a2a_status(TaskState::Running), A2aTaskStatus::Working);
        assert_eq!(a2a_status(TaskState::Cancelled), A2aTaskStatus::Canceled);
    }
}
