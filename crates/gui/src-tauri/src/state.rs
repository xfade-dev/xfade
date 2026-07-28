use agent_switch_core::Core;

pub struct AppState {
    pub core: Core,
}

impl AppState {
    pub fn new(core: Core) -> Self {
        Self { core }
    }
}
