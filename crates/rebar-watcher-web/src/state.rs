use std::sync::Arc;

use rebar_core::events::EventBus;
use rebar_core::process::table::ProcessTable;

#[derive(Clone)]
pub struct AppState {
    pub table: Arc<ProcessTable>,
    pub event_bus: EventBus,
}
