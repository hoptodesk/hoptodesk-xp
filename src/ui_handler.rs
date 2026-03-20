
use std::sync::{Arc, Mutex};

use crate::config;
use crate::signal;

pub struct AppState {
    pub config: config::Config,
    pub config2: config::Config2,
    pub local_config: config::LocalConfig,
    pub signal_state: Arc<Mutex<signal::SignalState>>,
    pub active_tab: String,
    pub sessions_dirty: bool,
}

impl AppState {
    pub fn new() -> Self {
        config::migrate_old_config();
        let cfg = config::Config::load();
        let cfg2 = config::Config2::load();
        let local = config::LocalConfig::load();
        crate::config::write_log(&format!("[config] ID={}, config dir={}", cfg.id, config::config_dir().display()));
        let saved_lang = cfg2.get_option("lang");
        if !saved_lang.is_empty() {
            crate::lang::set_lang(&saved_lang);
        }
        Self {
            config: cfg,
            config2: cfg2,
            local_config: local,
            signal_state: Arc::new(Mutex::new(signal::SignalState::default())),
            active_tab: "recent".to_string(),
            sessions_dirty: true,
        }
    }
}
