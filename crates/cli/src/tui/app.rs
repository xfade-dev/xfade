//! Testable state machine behind the TUI. Rendering lives in `ui.rs`;
//! everything here is pure logic over `Core` so it can be unit-tested
//! without a terminal.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use xfade_core::{Core, Provider, ToolKind};

/// Normalized knob positions (0.0 = far local end, 1.0 = far cloud end).
pub const FADER_LOCAL: f32 = 0.2;
pub const FADER_CLOUD: f32 = 0.8;
/// Braille spinner frames shown while a latency probe is in flight.
pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// How long the fader/status highlight lasts after a successful switch.
const FLASH_DURATION: Duration = Duration::from_millis(450);

/// Outcome of a latency probe against one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeResult {
    Ms(u128),
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    BaseUrl,
    Model,
    ApiKey,
}

#[derive(Debug, Clone)]
pub struct EditState {
    pub provider_id: String,
    pub field: EditField,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

pub struct App {
    pub tool: ToolKind,
    pub providers: Vec<Provider>,
    pub selected: usize,
    pub message: Option<String>,
    pub edit: Option<EditState>,
    /// provider id -> outcome of the latest `t` probe.
    pub probes: HashMap<String, ProbeResult>,
    /// Animated knob position; eased toward [`fader_target`] on every tick.
    pub fader_pos: f32,
    /// provider ids with in-flight probes (the `t` key probes all probeable
    /// providers of the current tool in parallel).
    pub probing: HashSet<String>,
    /// Frame counter driving the braille spinner.
    pub spinner_tick: usize,
    /// Switch-flash highlight deadline; cleared by [`tick`] once expired.
    pub flash_until: Option<Instant>,
}

impl App {
    pub fn new(tool: ToolKind, core: &Core) -> Self {
        let mut app = Self {
            tool,
            providers: Vec::new(),
            selected: 0,
            message: None,
            edit: None,
            probes: HashMap::new(),
            fader_pos: 0.5,
            probing: HashSet::new(),
            spinner_tick: 0,
            flash_until: None,
        };
        app.reload(core);
        // Snap to the target on open; only *changes* animate.
        app.fader_pos = app.fader_target();
        app
    }

    /// Refresh the provider list for the current tool; selection follows the
    /// active provider on first load and is clamped afterwards.
    pub fn reload(&mut self, core: &Core) {
        let previous = self.selected_provider().map(|p| p.id.clone());
        self.providers = core.list(Some(self.tool)).unwrap_or_default();
        self.selected = self
            .providers
            .iter()
            .position(|p| Some(&p.id) == previous.as_ref())
            .or_else(|| self.providers.iter().position(|p| p.is_active))
            .unwrap_or(0);
        if self.selected >= self.providers.len() {
            self.selected = self.providers.len().saturating_sub(1);
        }
    }

    pub fn next(&mut self) {
        if !self.providers.is_empty() {
            self.selected = (self.selected + 1) % self.providers.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.providers.is_empty() {
            self.selected = (self.selected + self.providers.len() - 1) % self.providers.len();
        }
    }

    pub fn next_tool(&mut self, core: &Core) {
        let idx = ToolKind::ALL
            .iter()
            .position(|t| *t == self.tool)
            .unwrap_or(0);
        self.tool = ToolKind::ALL[(idx + 1) % ToolKind::ALL.len()];
        // Drop state tied to the previous tool so `reload` doesn't try to carry
        // the old selection forward (ids can collide across tools), and probes
        // don't bleed into the new tool's providers.
        self.providers.clear();
        self.probes.clear();
        self.selected = 0;
        self.reload(core);
    }

    pub fn selected_provider(&self) -> Option<&Provider> {
        self.providers.get(self.selected)
    }

    pub fn active_provider(&self) -> Option<&Provider> {
        self.providers.iter().find(|p| p.is_active)
    }

    /// Where the knob *should* rest for the current active provider.
    fn fader_target(&self) -> f32 {
        if is_local_url(self.active_provider().and_then(|p| p.base_url.as_deref())) {
            FADER_LOCAL
        } else {
            FADER_CLOUD
        }
    }

    /// True while the fader is mid-slide or a probe is in flight — the event
    /// loop uses this to shorten its poll timeout for smooth frames.
    pub fn is_animating(&self) -> bool {
        (self.fader_pos - self.fader_target()).abs() > 0.005 || !self.probing.is_empty()
    }

    /// Advance all time-based state: fader easing, spinner frame, flash expiry.
    pub fn tick(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        let target = self.fader_target();
        let delta = target - self.fader_pos;
        if delta.abs() > 0.005 {
            self.fader_pos += delta * 0.35;
        } else {
            self.fader_pos = target;
        }
        if self
            .flash_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.flash_until = None;
        }
    }

    pub fn spinner_glyph(&self) -> char {
        SPINNER[self.spinner_tick % SPINNER.len()]
    }

    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    pub fn switch_selected(&mut self, core: &Core) {
        let Some(id) = self.selected_provider().map(|p| p.id.clone()) else {
            return;
        };
        self.message = Some(match core.use_provider(self.tool, &id) {
            Ok(()) => {
                self.flash_until = Some(Instant::now() + FLASH_DURATION);
                format!("Switched to {id}")
            }
            Err(e) => format!("Error: {e}"),
        });
        self.reload(core);
    }

    pub fn start_edit(&mut self) {
        let Some(p) = self.selected_provider() else {
            return;
        };
        self.edit = Some(EditState {
            provider_id: p.id.clone(),
            field: EditField::BaseUrl,
            base_url: p.base_url.clone().unwrap_or_default(),
            model: p.extra["model"].as_str().unwrap_or("").to_string(),
            api_key: String::new(),
        });
    }

    pub fn edit_char(&mut self, c: char) {
        if let Some(e) = self.edit.as_mut() {
            focused_field_mut(e).push(c);
        }
    }

    pub fn edit_backspace(&mut self) {
        if let Some(e) = self.edit.as_mut() {
            focused_field_mut(e).pop();
        }
    }

    pub fn edit_next_field(&mut self) {
        if let Some(e) = self.edit.as_mut() {
            e.field = match e.field {
                EditField::BaseUrl => EditField::Model,
                EditField::Model => EditField::ApiKey,
                EditField::ApiKey => EditField::BaseUrl,
            };
        }
    }

    pub fn edit_prev_field(&mut self) {
        if let Some(e) = self.edit.as_mut() {
            e.field = match e.field {
                EditField::BaseUrl => EditField::ApiKey,
                EditField::Model => EditField::BaseUrl,
                EditField::ApiKey => EditField::Model,
            };
        }
    }

    pub fn cancel_edit(&mut self) {
        self.edit = None;
    }

    pub fn submit_edit(&mut self, core: &Core) {
        let Some(e) = self.edit.take() else {
            return;
        };
        // Mutate the existing provider (not a fresh Provider::new) so keys like
        // `_original_model` / `remove_provider` in `extra` survive the edit.
        let Some(mut provider) = self
            .providers
            .iter()
            .find(|p| p.id == e.provider_id)
            .cloned()
        else {
            self.message = Some(format!("Error: {} not found", e.provider_id));
            return;
        };
        provider.base_url = match e.base_url.trim() {
            "" => None,
            s => Some(s.to_string()),
        };
        let mut map = provider.extra.as_object().cloned().unwrap_or_default();
        match e.model.trim() {
            "" => {
                map.remove("model");
            }
            m => {
                map.insert("model".into(), serde_json::Value::String(m.to_string()));
            }
        }
        provider.extra = serde_json::Value::Object(map);
        // Blank api key = keep the stored secret (pass None to update_provider).
        let api_key = match e.api_key.trim() {
            "" => None,
            s => Some(s.to_string()),
        };
        let was_active = provider.is_active;
        self.message = Some(match core.update_provider(&provider, api_key.as_deref()) {
            Ok(()) => format!("Saved {}", e.provider_id),
            Err(err) => format!("Error: {err}"),
        });
        // If the edited provider is the active route, re-apply the adapter so
        // the tool's live config tracks the change immediately.
        if was_active {
            if let Err(err) = core.use_provider(self.tool, &e.provider_id) {
                self.message = Some(format!("Saved, but re-apply failed: {err}"));
            }
        }
        self.reload(core);
    }
}

fn focused_field_mut(e: &mut EditState) -> &mut String {
    match e.field {
        EditField::BaseUrl => &mut e.base_url,
        EditField::Model => &mut e.model,
        EditField::ApiKey => &mut e.api_key,
    }
}

/// Loopback or RFC1918 private range: these count as the "local" side of the
/// fader; everything else (and official login, None) is the cloud side.
pub fn is_local_url(base_url: Option<&str>) -> bool {
    let Some(url) = base_url else {
        return false;
    };
    let Some((host, _)) = probe_target(url) else {
        return false;
    };
    is_loopback_or_private(&host)
}

fn is_loopback_or_private(host: &str) -> bool {
    if host == "localhost" || host == "::1" {
        return true;
    }
    // `parse::<u8>` must be explicit: on Windows, crossterm pulls in
    // `encode_unicode`, whose extra `FromIterator<u8>` impls make the
    // inferred collect target ambiguous (E0283/E0284).
    let octets: Vec<u8> = host
        .split('.')
        .filter_map(|s| s.parse::<u8>().ok())
        .collect();
    if octets.len() != 4 {
        return false;
    }
    match octets.as_slice() {
        [127, ..] => true,
        [10, ..] => true,
        [192, 168, ..] => true,
        [172, b, ..] if (16..=31).contains(b) => true,
        _ => false,
    }
}

/// TCP connect timing used by the `t` key. Returns elapsed milliseconds, or
/// None when the endpoint is unreachable within `timeout`. Runs in a worker
/// thread from the UI so the event loop never blocks.
pub fn probe_latency(host: &str, port: u16, timeout: std::time::Duration) -> Option<u128> {
    use std::net::{TcpStream, ToSocketAddrs};
    let addr = (host, port).to_socket_addrs().ok()?.next()?;
    let start = std::time::Instant::now();
    TcpStream::connect_timeout(&addr, timeout).ok()?;
    Some(start.elapsed().as_millis())
}

/// Parse `host:port` out of a base URL for the TCP latency probe.
/// Returns None when unparseable or the scheme has no known default port.
pub fn probe_target(base_url: &str) -> Option<(String, u16)> {
    let url = reqwest::Url::parse(base_url).ok()?;
    // url::Url's host_str() brackets IPv6 literals; normalize to the bare form.
    let host = url
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = url.port_or_known_default()?;
    Some((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use xfade_core::store::secrets::FileStore;

    fn test_core(dir: &tempfile::TempDir) -> Core {
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        Core::with_paths(
            &home,
            &data,
            Arc::new(FileStore::new(data.join("secrets.json"))),
        )
        .unwrap()
    }

    fn add(core: &Core, tool: ToolKind, id: &str, base_url: Option<&str>) {
        core.add_provider(
            Provider::new(id, tool, base_url.map(str::to_string)),
            Some("sk-test"),
        )
        .unwrap();
    }

    #[test]
    fn next_wraps_around_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        add(&core, ToolKind::ClaudeCode, "b", Some("http://y"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        assert_eq!(app.selected, 0);
        app.next();
        assert_eq!(app.selected, 1);
        app.next();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn prev_wraps_to_last_from_first() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        add(&core, ToolKind::ClaudeCode, "b", Some("http://y"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.prev();
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn new_selects_the_active_provider() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        add(&core, ToolKind::ClaudeCode, "b", Some("http://y"));
        core.use_provider(ToolKind::ClaudeCode, "b").unwrap();
        let app = App::new(ToolKind::ClaudeCode, &core);
        assert_eq!(app.selected_provider().map(|p| p.id.as_str()), Some("b"));
    }

    #[test]
    fn next_tool_cycles_through_all_tools_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::Codex, "cx", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        let mut seen = vec![app.tool];
        for _ in 0..ToolKind::ALL.len() {
            app.next_tool(&core);
            seen.push(app.tool);
        }
        // Full cycle returns to the starting tool.
        assert_eq!(seen.first(), seen.last());
        // Every tool was visited exactly once in between.
        let mut middle = seen[..seen.len() - 1].to_vec();
        middle.sort_by_key(|t| t.as_str());
        middle.dedup();
        assert_eq!(middle.len(), ToolKind::ALL.len());
        // Providers were reloaded for the final tool.
        assert!(app.providers.iter().all(|p| p.tool == app.tool));
    }

    #[test]
    fn next_tool_selects_new_tools_active_provider_not_colliding_id() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        // Same id "dup" in both tools, but with different roles. The old tool's
        // first provider id must not leak selection into the new tool.
        add(&core, ToolKind::ClaudeCode, "dup", Some("http://old"));
        add(&core, ToolKind::ClaudeCode, "z", Some("http://z"));
        add(&core, ToolKind::Codex, "dup", Some("http://new"));
        add(&core, ToolKind::Codex, "active-cx", Some("http://a"));
        core.use_provider(ToolKind::Codex, "active-cx").unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        assert_eq!(app.selected_provider().map(|p| p.id.as_str()), Some("dup"));
        app.next_tool(&core); // claude -> codex
        assert_eq!(
            app.selected_provider().map(|p| p.id.as_str()),
            Some("active-cx"),
            "tool switch must select the new tool's active provider, not the colliding id"
        );
    }

    #[test]
    fn next_tool_clears_probes_from_previous_tool() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "dup", Some("http://old"));
        add(&core, ToolKind::Codex, "dup", Some("http://new"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.probes.insert("dup".into(), ProbeResult::Ms(42));
        app.next_tool(&core);
        assert!(
            !app.probes.contains_key("dup"),
            "probes must not leak across tools with colliding ids"
        );
    }

    #[test]
    fn switch_selected_activates_and_sets_message() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        add(&core, ToolKind::ClaudeCode, "b", Some("http://y"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.next();
        app.switch_selected(&core);
        assert_eq!(
            app.active_provider().map(|p| p.id.as_str()),
            Some("b"),
            "provider b should be active after switch"
        );
        assert!(app.message.as_deref().unwrap_or("").contains('b'));
    }

    #[test]
    fn fader_snaps_to_cloud_target_on_open_without_animating() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(
            &core,
            ToolKind::ClaudeCode,
            "glm",
            Some("http://gw.example:3000"),
        );
        core.use_provider(ToolKind::ClaudeCode, "glm").unwrap();
        let app = App::new(ToolKind::ClaudeCode, &core);
        assert!(
            (app.fader_pos - FADER_CLOUD).abs() < 1e-6,
            "open must snap to cloud target, got {}",
            app.fader_pos
        );
        assert!(!app.is_animating());
    }

    #[test]
    fn fader_eases_to_local_target_after_switch() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(
            &core,
            ToolKind::ClaudeCode,
            "cloudy",
            Some("http://gw.example:3000"),
        );
        add(
            &core,
            ToolKind::ClaudeCode,
            "localy",
            Some("http://127.0.0.1:11434"),
        );
        core.use_provider(ToolKind::ClaudeCode, "cloudy").unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.next(); // select localy
        app.switch_selected(&core);
        assert!(app.is_animating(), "switch must start a slide");
        let mut positions = vec![app.fader_pos];
        for _ in 0..60 {
            app.tick();
            positions.push(app.fader_pos);
        }
        assert!(
            positions.windows(2).any(|w| (w[0] - w[1]).abs() > 1e-4),
            "knob must move across frames: {positions:?}"
        );
        assert!(
            (app.fader_pos - FADER_LOCAL).abs() < 1e-3,
            "knob must settle on the local target, got {}",
            app.fader_pos
        );
        assert!(!app.is_animating());
    }

    #[test]
    fn successful_switch_sets_flash_and_tick_expires_it() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        add(&core, ToolKind::ClaudeCode, "b", Some("http://y"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.next();
        app.switch_selected(&core);
        assert!(app.flash_until.is_some(), "flash must follow a switch");
        app.flash_until = Some(Instant::now() - Duration::from_millis(1));
        app.tick();
        assert!(app.flash_until.is_none(), "expired flash must clear");
    }

    #[test]
    fn tick_cycles_the_spinner_glyph() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        let first = app.spinner_glyph();
        app.tick();
        assert_ne!(first, app.spinner_glyph(), "spinner must advance");
        for _ in 0..(SPINNER.len() - 1) {
            app.tick();
        }
        assert_eq!(
            app.spinner_glyph(),
            first,
            "spinner must wrap the full cycle"
        );
    }

    #[test]
    fn start_edit_prefills_from_selected_provider() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut p = Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into()));
        p.extra = serde_json::json!({"model": "glm-4.6"});
        core.add_provider(p, Some("sk-test")).unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_edit();
        let edit = app.edit.as_ref().expect("edit state");
        assert_eq!(edit.provider_id, "glm");
        assert_eq!(edit.base_url, "http://gw:3000");
        assert_eq!(edit.model, "glm-4.6");
        assert!(edit.api_key.is_empty(), "api key starts blank = keep");
        assert_eq!(edit.field, EditField::BaseUrl);
    }

    #[test]
    fn edit_char_and_backspace_touch_only_focused_field() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_edit();
        app.edit_char('!');
        assert!(app.edit.as_ref().unwrap().base_url.ends_with('!'));
        assert_eq!(app.edit.as_ref().unwrap().model, "");
        app.edit_backspace();
        assert_eq!(app.edit.as_ref().unwrap().base_url, "http://x");
        app.edit_next_field(); // -> Model
        app.edit_char('m');
        assert_eq!(app.edit.as_ref().unwrap().model, "m");
        assert_eq!(app.edit.as_ref().unwrap().base_url, "http://x");
    }

    #[test]
    fn edit_next_field_cycles_base_url_model_apikey() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_edit();
        assert_eq!(app.edit.as_ref().unwrap().field, EditField::BaseUrl);
        app.edit_next_field();
        assert_eq!(app.edit.as_ref().unwrap().field, EditField::Model);
        app.edit_next_field();
        assert_eq!(app.edit.as_ref().unwrap().field, EditField::ApiKey);
        app.edit_next_field();
        assert_eq!(app.edit.as_ref().unwrap().field, EditField::BaseUrl);
    }

    #[test]
    fn edit_prev_field_moves_backwards() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_edit();
        assert_eq!(app.edit.as_ref().unwrap().field, EditField::BaseUrl);
        app.edit_prev_field();
        assert_eq!(app.edit.as_ref().unwrap().field, EditField::ApiKey);
        app.edit_prev_field();
        assert_eq!(app.edit.as_ref().unwrap().field, EditField::Model);
        app.edit_prev_field();
        assert_eq!(app.edit.as_ref().unwrap().field, EditField::BaseUrl);
    }

    #[test]
    fn cancel_edit_discards_and_returns_to_list() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_edit();
        app.edit_char('z');
        app.cancel_edit();
        assert!(!app.is_editing());
        let p = &app.providers[0];
        assert_eq!(p.base_url.as_deref(), Some("http://x"));
    }

    #[test]
    fn submit_edit_updates_base_url_and_model() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_edit();
        {
            let e = app.edit.as_mut().unwrap();
            e.base_url = "http://new:8080".into();
            e.model = "m-1".into();
        }
        app.submit_edit(&core);
        assert!(!app.is_editing());
        let p = &app.providers[0];
        assert_eq!(p.base_url.as_deref(), Some("http://new:8080"));
        assert_eq!(p.extra["model"].as_str(), Some("m-1"));
    }

    #[test]
    fn submit_edit_blank_base_url_switches_to_official() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_edit();
        app.edit.as_mut().unwrap().base_url = "   ".into();
        app.submit_edit(&core);
        assert!(app.providers[0].is_official());
    }

    #[test]
    fn submit_edit_blank_api_key_keeps_existing_key() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        let before = core.secrets().get("xfade/claude/a").unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_edit();
        app.submit_edit(&core);
        let after = core.secrets().get("xfade/claude/a").unwrap();
        assert_eq!(before, after, "blank api key must not rotate the secret");
        assert_eq!(after, "sk-test");
    }

    #[test]
    fn submit_edit_preserves_original_capture_keys() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut p = Provider::new("cx", ToolKind::Codex, Some("http://gw:3000".into()));
        p.extra = serde_json::json!({
            "model": "glm-4.6",
            "_original_model": "gpt-5",
            "_original_context_window": 200000
        });
        core.add_provider(p, Some("sk-test")).unwrap();
        let mut app = App::new(ToolKind::Codex, &core);
        app.start_edit();
        {
            let e = app.edit.as_mut().unwrap();
            e.base_url = "http://new:8080".into();
            e.model = "glm-4.7".into();
        }
        app.submit_edit(&core);
        let p = &app.providers[0];
        assert_eq!(p.extra["model"], "glm-4.7", "model must be updated");
        assert_eq!(
            p.extra["_original_model"], "gpt-5",
            "_original_* must survive"
        );
        assert_eq!(p.extra["_original_context_window"], 200000);
    }

    #[test]
    fn submit_edit_reapplies_adapter_when_active() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        core.use_provider(ToolKind::ClaudeCode, "a").unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_edit();
        {
            let e = app.edit.as_mut().unwrap();
            e.base_url = "http://new:8080".into();
        }
        app.submit_edit(&core);
        // The adapter config on disk must reflect the new base_url.
        let written = std::fs::read_to_string(
            dir.path()
                .join("home")
                .join(".claude")
                .join("settings.json"),
        )
        .unwrap();
        assert!(
            written.contains("new:8080"),
            "active provider's live config must be re-applied, got: {written}"
        );
    }

    #[test]
    fn is_local_url_detects_loopback_and_treats_official_as_cloud() {
        assert!(is_local_url(Some("http://127.0.0.1:24860")));
        assert!(is_local_url(Some("http://localhost:11434/v1")));
        assert!(is_local_url(Some("http://[::1]:8080")));
        assert!(is_local_url(Some("http://192.168.1.10:11434")));
        assert!(is_local_url(Some("http://10.0.0.5:3000")));
        assert!(is_local_url(Some("http://172.16.3.4:8080")));
        assert!(!is_local_url(Some("https://api.deepseek.com")));
        assert!(!is_local_url(Some("https://8.8.8.8")));
        assert!(!is_local_url(None), "official login counts as cloud side");
    }

    #[test]
    fn probe_target_parses_host_and_default_ports() {
        assert_eq!(
            probe_target("http://127.0.0.1:24860"),
            Some(("127.0.0.1".into(), 24860))
        );
        assert_eq!(
            probe_target("https://api.deepseek.com/v1"),
            Some(("api.deepseek.com".into(), 443))
        );
        assert_eq!(
            probe_target("http://localhost:11434"),
            Some(("localhost".into(), 11434))
        );
        assert_eq!(
            probe_target("http://[::1]:8080"),
            Some(("::1".into(), 8080))
        );
        assert_eq!(probe_target("not a url"), None);
    }

    #[test]
    fn probe_latency_measures_a_listening_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ms = probe_latency("127.0.0.1", port, std::time::Duration::from_millis(500));
        assert!(ms.is_some(), "probe to a live listener should succeed");
    }

    #[test]
    fn probe_latency_returns_none_for_a_closed_port() {
        // Bind then drop to get a port that is closed again.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(probe_latency("127.0.0.1", port, std::time::Duration::from_millis(200)).is_none());
    }
}
