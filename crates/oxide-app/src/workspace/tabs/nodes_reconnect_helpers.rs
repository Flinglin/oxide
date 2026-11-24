use super::*;

pub(super) fn event_log_severity_for_connection_status(status: &str) -> WorkspaceEventSeverity {
    match status {
        // Mirrors Tauri `useEventLogCapture.statusSeverity`: link loss is the
        // disruptive event, while a final explicit disconnect is informational.
        "link_down" => WorkspaceEventSeverity::Error,
        "reconnecting" => WorkspaceEventSeverity::Warn,
        "connected" | "disconnected" => WorkspaceEventSeverity::Info,
        _ => WorkspaceEventSeverity::Info,
    }
}

pub(super) fn event_log_title_for_node_readiness(readiness: &NodeReadiness) -> &'static str {
    match readiness {
        NodeReadiness::Ready => "event_log.events.node_state_ready",
        NodeReadiness::Connecting => "event_log.events.node_state_connecting",
        NodeReadiness::Error => "event_log.events.node_state_error",
        NodeReadiness::Disconnected => "event_log.events.node_state_disconnected",
    }
}

pub(super) fn node_readiness_became_ready(
    previous: Option<&NodeReadiness>,
    current: &NodeReadiness,
) -> bool {
    !matches!(previous, Some(NodeReadiness::Ready)) && matches!(current, NodeReadiness::Ready)
}

pub(super) fn node_readiness_became_unavailable(
    previous: Option<&NodeReadiness>,
    current: &NodeReadiness,
) -> bool {
    !matches!(
        previous,
        Some(NodeReadiness::Error | NodeReadiness::Disconnected)
    ) && matches!(current, NodeReadiness::Error | NodeReadiness::Disconnected)
}

pub(super) fn reconnect_cascade_child_should_start(readiness: &NodeReadiness) -> bool {
    matches!(readiness, NodeReadiness::Error | NodeReadiness::Connecting)
}
