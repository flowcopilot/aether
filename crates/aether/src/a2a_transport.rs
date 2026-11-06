use aether_core::{EndpointId, ScopeKind, TrustProvider};

use aether_store::StoreError;

use crate::{
    execution::{Agent, Task},
    proof::{Identity, ProofError, ProofHandle, SubmissionInput},
    transport::{
        A2aCancel, A2aPartContent, A2aRequest, A2aTaskStatus, Frame, FrameKind,
        MAX_EXCHANGE_BYTES, MAX_FRAMES, TransportError, event_payload,
        text_result_payload, text_result_payload_with_progress,
    },
};

const EVENT_PAGE_BYTE_BUDGET: usize = MAX_EXCHANGE_BYTES - 65_536;

pub struct A2aTransport<'a, T, P> {
    proof: ProofHandle<'a, T, P>,
}

impl<'a, T, P> A2aTransport<'a, T, P> {
    #[must_use]
    pub const fn new(proof: ProofHandle<'a, T, P>) -> Self {
        Self { proof }
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
            skill: skill_for_direct_a2a_entry(
                self.proof.scope_kind_for(&frame.scope_id),
                request.capability_id.as_str(),
            ),
            workspace_id: request.workspace_id.as_str().to_owned(),
            remote_text: text.to_owned(),
        };
        let admission = self
            .proof
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
            .proof
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
            && self.proof.content_is_expired(&task_id).map_err(map_error)?
        {
            return Err(TransportError::ResultExpired);
        }
        let status = a2a_status(state);
        let result_text = wire_result_text(state, result)?;
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
        self.proof
            .cancel_from(
                task_id,
                &EndpointId::new(authenticated_endpoint),
                &frame.scope_id,
            )
            .map_err(map_error)?;
        let event = self
            .proof
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

fn wire_result_text(
    state: aether_store::TaskState,
    result: Option<String>,
) -> Result<String, TransportError> {
    if state == aether_store::TaskState::Succeeded {
        result.ok_or_else(|| TransportError::Network("task result missing".to_owned()))
    } else {
        Ok(String::new())
    }
}

fn skill_for_direct_a2a_entry(kind: Option<ScopeKind>, capability: &str) -> String {
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
        ProofError::TaskMismatch => TransportError::BadFrame,
        ProofError::Shutdown => {
            TransportError::Network("node shutting down".to_owned())
        }
        ProofError::Execution(_) => {
            TransportError::Network("task execution failed".to_owned())
        }
        ProofError::Internal => {
            TransportError::Network("node internal error".to_owned())
        }
        ProofError::Store(_) => {
            TransportError::Network("task store unavailable".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use aether_store::{EventType, TaskState};

    use crate::{
        execution::ExecutionError,
        transport::{A2aTaskStatus, TransportError},
    };

    use super::{
        event_status, map_error, skill_for_direct_a2a_entry, wire_result_text,
    };

    #[test]
    fn a_pact_task_keeps_its_brand_capability() {
        assert_eq!(
            skill_for_direct_a2a_entry(Some(aether_core::ScopeKind::Pact), "review"),
            "review"
        );
        assert_eq!(
            skill_for_direct_a2a_entry(Some(aether_core::ScopeKind::Direct), "review"),
            crate::execution::GENERAL_SKILL
        );
    }

    #[test]
    fn map_error_distinguishes_shutdown_and_internal_from_execution() {
        assert!(matches!(
            map_error(crate::proof::ProofError::Shutdown),
            TransportError::Network(message) if message == "node shutting down"
        ));
        assert!(matches!(
            map_error(crate::proof::ProofError::Internal),
            TransportError::Network(message) if message == "node internal error"
        ));
        assert!(matches!(
            map_error(crate::proof::ProofError::Execution(ExecutionError::Path)),
            TransportError::Network(message) if message == "task execution failed"
        ));
        assert_eq!(
            map_error(crate::proof::ProofError::Store(
                aether_store::StoreError::Overloaded
            )),
            TransportError::Overloaded
        );
    }

    #[test]
    fn succeeded_task_without_stored_result_is_a_wire_error() {
        assert_eq!(
            wire_result_text(TaskState::Succeeded, Some("done".to_owned()))
                .expect("present"),
            "done"
        );
        assert!(matches!(
            wire_result_text(TaskState::Succeeded, None),
            Err(TransportError::Network(message)) if message == "task result missing"
        ));
        assert_eq!(
            wire_result_text(TaskState::Running, None).expect("non-terminal"),
            ""
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
    }
}
