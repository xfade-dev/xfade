use crate::proxy_ctl::ProxyCtl;
use agent_switch_core::Core;
use std::sync::Arc;

pub struct AppState {
    pub core: Core,
    pub proxy: Arc<ProxyCtl>,
}

impl AppState {
    pub fn new(core: Core) -> Self {
        Self {
            core,
            proxy: Arc::new(ProxyCtl::default()),
        }
    }
}
