use std::{
    sync::Mutex,
    sync::mpsc::{self, SyncSender},
    thread::{self, JoinHandle},
};

use aether_store::{
    NodeStore, QueueLimits, StoreError, TaskAdmission as StoreAdmission, TaskInput,
};

use super::{ProofError, REPLAY_LIMIT, replay_expiry};

type StoreCommand = Box<dyn FnOnce(&mut NodeStore) + Send>;

enum StoreMessage {
    Operation(StoreCommand),
    Stop,
}

pub(in crate::proof) struct StoreWorker {
    sender: SyncSender<StoreMessage>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Clone for StoreWorker {
    fn clone(&self) -> Self {
        Self { sender: self.sender.clone(), thread: Mutex::new(None) }
    }
}

impl StoreWorker {
    pub(in crate::proof) fn new(store: NodeStore) -> Result<Self, ProofError> {
        let (sender, receiver) = mpsc::sync_channel::<StoreMessage>(64);
        let thread = thread::Builder::new()
            .name("aether-store".to_owned())
            .spawn(move || {
                let mut store = store;
                while let Ok(StoreMessage::Operation(command)) = receiver.recv() {
                    command(&mut store);
                }
            })
            .map_err(|_| ProofError::Internal)?;
        Ok(Self { sender, thread: Mutex::new(Some(thread)) })
    }

    pub(in crate::proof) fn call<R: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut NodeStore) -> Result<R, StoreError> + Send + 'static,
    ) -> Result<R, ProofError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(StoreMessage::Operation(Box::new(move |store| {
                let _ = sender.send(operation(store));
            })))
            .map_err(|_| ProofError::Internal)?;
        receiver.recv().map_err(|_| ProofError::Internal)?.map_err(ProofError::Store)
    }

    pub(in crate::proof) fn admit_only(
        &self,
        input: TaskInput,
        limits: QueueLimits,
        replay_key: Option<String>,
    ) -> Result<StoreAdmission, ProofError> {
        self.call(move |store| match replay_key {
            Some(replay_key) => store.admit_with_replay(
                &input,
                limits,
                &replay_key,
                replay_expiry(),
                REPLAY_LIMIT,
            ),
            None => store.admit(&input, limits),
        })
    }

    pub(in crate::proof) fn stop(&self) {
        let _ = self.sender.send(StoreMessage::Stop);
    }

    pub(in crate::proof) fn join(&self) {
        self.stop();
        if let Ok(mut guard) = self.thread.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
    }
}
