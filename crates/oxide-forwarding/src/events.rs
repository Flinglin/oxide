use std::{
    fmt,
    sync::{
        Arc,
        mpsc::{SendError, Sender},
    },
};

use serde::{Deserialize, Serialize};

use crate::{DetectedPort, ForwardStats, ForwardStatus};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ForwardEvent {
    StatusChanged {
        forward_id: String,
        session_id: String,
        status: ForwardStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    StatsUpdated {
        forward_id: String,
        session_id: String,
        stats: ForwardStats,
    },
    SessionSuspended {
        session_id: String,
        forward_ids: Vec<String>,
    },
    PortDetected {
        connection_id: String,
        new_ports: Vec<DetectedPort>,
        closed_ports: Vec<DetectedPort>,
        all_ports: Vec<DetectedPort>,
    },
}

#[derive(Clone)]
pub struct ForwardEventDeliverySender {
    sender: Sender<ForwardEvent>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl ForwardEventDeliverySender {
    pub fn new(sender: Sender<ForwardEvent>) -> Self {
        Self { sender, wake: None }
    }

    pub fn with_wake(sender: Sender<ForwardEvent>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            sender,
            wake: Some(wake),
        }
    }

    pub fn send(&self, event: ForwardEvent) -> Result<(), SendError<ForwardEvent>> {
        self.sender.send(event)?;
        if let Some(wake) = &self.wake {
            wake();
        }
        Ok(())
    }
}

impl fmt::Debug for ForwardEventDeliverySender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForwardEventDeliverySender")
            .field("has_wake", &self.wake.is_some())
            .finish_non_exhaustive()
    }
}
