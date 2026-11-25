//! Testable state machine behind the TUI. Rendering lives in `ui.rs`;
//! everything here is pure logic over `Core` so it can be unit-tested
//! without a terminal.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use xfade_core::{AddProviderInput, Core, Provider, ProviderProtocol, SecretsBackend, ToolKind};

/// Normalized knob positions (0.0 = far local end, 1.0 = far cloud end).
pub const FADER_LOCAL: f32 = 0.2;
pub const FADER_CLOUD: f32 = 0.8;
/// Braille spinner frames shown while a latency probe is in flight.
pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// How long the fader/status highlight lasts after a successful switch.
const FLASH_DURATION: Duration = Duration::from_millis(450);
/// How long the open animation runs: the header gradients fade in while the
/// knob eases from center to the active provider's side. Long enough to be
/// clearly visible on launch — the terminal's alt-screen switch would otherwise
/// mask a shorter intro.
const INTRO_DURATION: Duration = Duration::from_millis(1200);

/// Fixed-choice fields are picked with Left/Right/Space instead of typed, so a
/// first-time user sees every valid value. Each entry is `(stored, shown)`.
pub const SECRETS_CHOICES: [(&str, &str); 2] = [("keyring", "keyring"), ("file", "file")];
pub const API_CHOICES: [(&str, &str); 2] = [
    ("openai-completions", "openai-completions"),
    ("anthropic-messages", "anthropic-messages"),
];
/// Pre-selected API type; matches what Pi/OMP/OpenClaw fall back to when unset.
const DEFAULT_API: &str = "openai-completions";
pub const PROTOCOL_CHOICES: [(&str, &str); 4] = [
    ("", "auto"),
    ("chat", "chat"),
    ("messages", "messages"),
    ("responses", "responses"),
];

/// Next/previous stored value in `choices`, wrapping. A current value outside
/// the list (e.g. hand-edited `config.json`) steps onto the first/last entry.
fn cycle_choice(choices: &[(&str, &str)], current: &str, forward: bool) -> String {
    let n = choices.len();
    let next = match choices.iter().position(|(v, _)| *v == current) {
        Some(i) if forward => (i + 1) % n,
        Some(i) => (i + n - 1) % n,
        None if forward => 0,
        None => n - 1,
    };
    choices[next].0.to_string()
}

/// Outcome of a latency probe against one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeResult {
    Ms(u128),
    Failed,
}

/// Outcome of a `/v1/models` catalog probe against one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelProbeResult {
    Models(Vec<String>),
    Failed(String),
}

/// Cycle through discovered model candidates when the user presses Left/Right on
/// a model field. Returns `None` when no candidates are available.
pub fn cycle_model_candidate(
    candidates: &[String],
    current: &str,
    forward: bool,
) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let n = candidates.len();
    let next = match candidates.iter().position(|m| m == current) {
        Some(i) if forward => (i + 1) % n,
        Some(i) => (i + n - 1) % n,
        None if forward => 0,
        None => n - 1,
    };
    Some(candidates[next].clone())
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
    pub available_models: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddField {
    Id,
    BaseUrl,
    Model,
    ApiKey,
    Protocol,
    Official,
}

#[derive(Debug, Clone)]
pub struct AddState {
    pub field: AddField,
    pub id: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub protocol: String,
    pub official: bool,
    /// Global defaults shown as placeholders. They are applied by `Core` when a
    /// field is left blank, not prefilled, so a blank field never looks like
    /// an editable value that silently comes back.
    pub global_base_url: Option<String>,
    pub global_model: Option<String>,
    pub available_models: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigField {
    Secrets,
    BaseUrl,
    Model,
    Api,
    ApiKey,
}

#[derive(Debug, Clone)]
pub struct ConfigState {
    pub field: ConfigField,
    /// Backend this process is really using (not merely what `config.json`
    /// says), so a pending backend switch can't send a key to the old store.
    pub active_secrets: SecretsBackend,
    pub secrets: String,
    pub base_url: String,
    pub model: String,
    pub api: String,
    pub api_key: String,
    pub api_key_is_set: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelMapField {
    Alias,
    Provider,
    UpstreamModel,
}

#[derive(Debug, Clone)]
pub struct ModelMapEdit {
    pub field: ModelMapField,
    pub alias: String,
    pub provider_id: String,
    pub upstream_model: String,
    pub available_models: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ModelMapState {
    pub models_merge: bool,
    pub selected_row: usize,
    pub rows: Vec<(String, String, String)>,
    pub editing: Option<ModelMapEdit>,
    pub provider_choices: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RouteEntry {
    pub provider_id: String,
    pub enabled: bool,
    pub model: String,
    pub available_models: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RoutesState {
    pub entries: Vec<RouteEntry>,
    pub selected_row: usize,
    pub editing_model: bool,
    pub target_protocol: String,
    pub model_override: Option<String>,
}

pub struct App {
    pub tool: ToolKind,
    pub providers: Vec<Provider>,
    pub selected: usize,
    pub message: Option<String>,
    pub edit: Option<EditState>,
    pub add: Option<AddState>,
    pub config_edit: Option<ConfigState>,
    pub model_map: Option<ModelMapState>,
    pub routes_edit: Option<RoutesState>,
    pub delete_confirm: Option<String>,
    /// provider id -> outcome of the latest `t` probe.
    pub probes: HashMap<String, ProbeResult>,
    /// Animated knob position; eased toward [`fader_target`] on every tick.
    pub fader_pos: f32,
    /// provider ids with in-flight probes (the `t` key probes all probeable
    /// providers of the current tool in parallel).
    pub probing: HashSet<String>,
    /// provider id -> outcome of the latest `p` (`/v1/models`) probe.
    pub model_probes: HashMap<String, ModelProbeResult>,
    /// provider ids with in-flight `/v1/models` probes.
    pub model_probing: HashSet<String>,
    /// Frame counter driving the braille spinner.
    pub spinner_tick: usize,
    /// Trust status of the environment
    pub trust: xfade_core::security::TrustState,
    /// Timestamp of last trust state refresh (throttles expensive security scans)
    pub last_trust_refresh: Instant,
    /// Switch-flash highlight deadline; cleared by [`tick`] once expired.
    pub flash_until: Option<Instant>,
    /// Open-animation deadline; the header fades in and the knob slides until
    /// then. Cleared by [`tick`] once finished.
    pub intro_until: Option<Instant>,
    /// Set on the first key press; once true the knob always uses the snappy
    /// switch easing, so provider switches made during the intro window don't
    /// inherit the slow opening glide.
    pub user_interacted: bool,
}

impl App {
    pub fn new(tool: ToolKind, core: &Core) -> Self {
        let mut app = Self {
            tool,
            providers: Vec::new(),
            selected: 0,
            message: None,
            edit: None,
            add: None,
            config_edit: None,
            model_map: None,
            routes_edit: None,
            delete_confirm: None,
            probes: HashMap::new(),
            fader_pos: 0.5,
            probing: HashSet::new(),
            model_probes: HashMap::new(),
            model_probing: HashSet::new(),
            spinner_tick: 0,
            trust: core
                .trust_state()
                .unwrap_or(xfade_core::security::TrustState::Guarded),
            last_trust_refresh: Instant::now(),
            flash_until: None,
            intro_until: None,
            user_interacted: false,
        };
        app.reload(core);
        // Start centered and fade the header in, so the knob eases to the
        // active provider's side on open — a brief "xfade resolves the route"
        // intro instead of snapping straight to the target.
        app.intro_until = Some(Instant::now() + INTRO_DURATION);
        app
    }

    /// Refresh the provider list for the current tool; selection follows the
    /// active provider on first load and is clamped afterwards.
    pub fn reload(&mut self, core: &Core) {
        let previous = self.selected_provider().map(|p| p.id.clone());
        self.providers = core.list(Some(self.tool)).unwrap_or_default();
        if self.last_trust_refresh.elapsed() >= std::time::Duration::from_secs(3) {
            self.trust = core
                .trust_state()
                .unwrap_or(xfade_core::security::TrustState::Guarded);
            self.last_trust_refresh = Instant::now();
        }
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
        self.probing.clear();
        self.model_probes.clear();
        self.model_probing.clear();
        self.message = None;
        self.selected = 0;
        self.reload(core);
    }

    pub fn prev_tool(&mut self, core: &Core) {
        let idx = ToolKind::ALL
            .iter()
            .position(|t| *t == self.tool)
            .unwrap_or(0);
        let n = ToolKind::ALL.len();
        self.tool = ToolKind::ALL[(idx + n - 1) % n];
        self.providers.clear();
        self.probes.clear();
        self.probing.clear();
        self.model_probes.clear();
        self.model_probing.clear();
        self.message = None;
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

    /// True while the fader is mid-slide, a probe is in flight, the intro is
    /// running, or a switch flash is showing — the event loop uses this to
    /// shorten its poll timeout and keep repainting until every transient
    /// (including the flash clearing) has actually been rendered.
    pub fn is_animating(&self) -> bool {
        (self.fader_pos - self.fader_target()).abs() > 0.005
            || !self.probing.is_empty()
            || !self.model_probing.is_empty()
            || self.intro_until.is_some()
            || self.flash_until.is_some()
    }

    /// 0.0 at open → 1.0 once the intro finishes. The header gradients use it
    /// to fade in from dark; it reads as 1.0 after the intro completes.
    pub fn intro_progress(&self) -> f32 {
        match self.intro_until {
            None => 1.0,
            Some(until) => {
                let remaining = until.saturating_duration_since(Instant::now());
                let elapsed = INTRO_DURATION.saturating_sub(remaining);
                (elapsed.as_secs_f32() / INTRO_DURATION.as_secs_f32()).clamp(0.0, 1.0)
            }
        }
    }

    /// Advance all time-based state: fader easing, spinner frame, flash expiry.
    pub fn tick(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        let target = self.fader_target();
        let delta = target - self.fader_pos;
        // The opening glide eases more slowly than a mid-session switch, but
        // only until the user interacts — switches made during the intro
        // window must stay snappy.
        let factor = if self.intro_until.is_some() && !self.user_interacted {
            0.08
        } else {
            0.35
        };
        if delta.abs() > 0.005 {
            self.fader_pos += delta * factor;
        } else {
            self.fader_pos = target;
        }
        if self
            .flash_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.flash_until = None;
        }
        if self
            .intro_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.intro_until = None;
        }
    }

    pub fn spinner_glyph(&self) -> char {
        SPINNER[self.spinner_tick % SPINNER.len()]
    }

    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    pub fn is_adding(&self) -> bool {
        self.add.is_some()
    }

    pub fn is_configuring(&self) -> bool {
        self.config_edit.is_some()
    }

    pub fn is_model_mapping(&self) -> bool {
        self.model_map.is_some()
    }

    pub fn is_routing(&self) -> bool {
        self.routes_edit.is_some()
    }

    pub fn is_input_mode(&self) -> bool {
        self.is_editing()
            || self.is_adding()
            || self.is_configuring()
            || self.is_model_mapping()
            || self.is_routing()
    }

    pub fn switch_selected(&mut self, core: &Core) {
        let Some(id) = self.selected_provider().map(|p| p.id.clone()) else {
            return;
        };
        self.message = Some(match core.use_provider(self.tool, &id, false) {
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
        let available_models = match self.model_probes.get(&p.id) {
            Some(ModelProbeResult::Models(ms)) => ms.clone(),
            _ => Vec::new(),
        };
        self.edit = Some(EditState {
            provider_id: p.id.clone(),
            field: EditField::BaseUrl,
            base_url: p.base_url.clone().unwrap_or_default(),
            model: p.extra["model"].as_str().unwrap_or("").to_string(),
            api_key: String::new(),
            available_models,
        });
    }

    pub fn start_add(&mut self, core: &Core) {
        let config = core.config();
        let mut seen = HashSet::new();
        let mut available_models = Vec::new();
        for res in self.model_probes.values() {
            if let ModelProbeResult::Models(ms) = res {
                for m in ms {
                    if seen.insert(m.clone()) {
                        available_models.push(m.clone());
                    }
                }
            }
        }
        self.add = Some(AddState {
            field: AddField::Id,
            id: String::new(),
            base_url: String::new(),
            model: String::new(),
            api_key: String::new(),
            protocol: String::new(),
            official: false,
            global_base_url: config.base_url.filter(|u| !u.trim().is_empty()),
            global_model: config.model.filter(|m| !m.trim().is_empty()),
            available_models,
        });
        self.message = None;
    }

    pub fn start_config(&mut self, core: &Core) {
        let config = core.config();
        let api_key_is_set = core.global_api_key().is_some();
        self.config_edit = Some(ConfigState {
            field: ConfigField::Secrets,
            active_secrets: core.active_secrets_backend().unwrap_or(config.secrets),
            secrets: config.secrets.to_string(),
            base_url: config.base_url.unwrap_or_default(),
            model: config.model.unwrap_or_default(),
            api: config.api.unwrap_or_else(|| DEFAULT_API.to_string()),
            api_key: String::new(),
            api_key_is_set,
        });
        self.message = None;
    }

    fn models_for_provider_choice(
        model_probes: &HashMap<String, ModelProbeResult>,
        provider_id: &str,
    ) -> Vec<String> {
        if provider_id != "*" {
            if let Some(ModelProbeResult::Models(ms)) = model_probes.get(provider_id) {
                return ms.clone();
            }
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for res in model_probes.values() {
            if let ModelProbeResult::Models(ms) = res {
                for m in ms {
                    if seen.insert(m.clone()) {
                        out.push(m.clone());
                    }
                }
            }
        }
        out.sort();
        out
    }

    fn load_model_map_rows(core: &Core) -> (Vec<(String, String, String)>, bool) {
        let mappings = core.db().get_model_mappings().unwrap_or_default();
        let mut rows = Vec::new();
        for (alias, prov_map) in mappings {
            for (prov, upstream) in prov_map {
                rows.push((alias.clone(), prov, upstream));
            }
        }
        let merge = core.db().models_merge_enabled().unwrap_or(false);
        (rows, merge)
    }

    pub fn start_model_map(&mut self, core: &Core) {
        let (rows, models_merge) = Self::load_model_map_rows(core);
        let mut provider_choices = vec!["*".to_string()];
        for p in &self.providers {
            if !provider_choices.contains(&p.id) {
                provider_choices.push(p.id.clone());
            }
        }
        self.model_map = Some(ModelMapState {
            models_merge,
            selected_row: 0,
            rows,
            editing: None,
            provider_choices,
        });
        self.message = None;
    }

    pub fn model_map_next(&mut self) {
        if let Some(mm) = self.model_map.as_mut() {
            if !mm.rows.is_empty() {
                mm.selected_row = (mm.selected_row + 1) % mm.rows.len();
            }
        }
    }

    pub fn model_map_prev(&mut self) {
        if let Some(mm) = self.model_map.as_mut() {
            if !mm.rows.is_empty() {
                mm.selected_row = (mm.selected_row + mm.rows.len() - 1) % mm.rows.len();
            }
        }
    }

    pub fn model_map_start_add(&mut self) {
        let default_provider = self
            .selected_provider()
            .map(|p| p.id.clone())
            .unwrap_or_else(|| "*".to_string());
        let available_models =
            Self::models_for_provider_choice(&self.model_probes, &default_provider);
        if let Some(mm) = self.model_map.as_mut() {
            mm.editing = Some(ModelMapEdit {
                field: ModelMapField::Alias,
                alias: String::new(),
                provider_id: default_provider,
                upstream_model: String::new(),
                available_models,
            });
        }
    }

    pub fn model_map_start_edit(&mut self) {
        let Some((alias, provider_id, upstream_model)) = self
            .model_map
            .as_ref()
            .and_then(|mm| mm.rows.get(mm.selected_row).cloned())
        else {
            return;
        };
        let available_models = Self::models_for_provider_choice(&self.model_probes, &provider_id);
        if let Some(mm) = self.model_map.as_mut() {
            mm.editing = Some(ModelMapEdit {
                field: ModelMapField::UpstreamModel,
                alias,
                provider_id,
                upstream_model,
                available_models,
            });
        }
    }

    pub fn model_map_delete(&mut self, core: &Core) {
        let Some((alias, provider_id, _)) = self
            .model_map
            .as_ref()
            .and_then(|mm| mm.rows.get(mm.selected_row).cloned())
        else {
            return;
        };
        match core.db().remove_model_mapping(&alias, Some(&provider_id)) {
            Ok(true) => {
                let (rows, models_merge) = Self::load_model_map_rows(core);
                if let Some(mm) = self.model_map.as_mut() {
                    mm.rows = rows;
                    mm.models_merge = models_merge;
                    if mm.selected_row >= mm.rows.len() {
                        mm.selected_row = mm.rows.len().saturating_sub(1);
                    }
                }
                self.message = Some(format!("Removed mapping {alias} ({provider_id})"));
            }
            Ok(false) => {
                self.message = Some("Mapping not found".into());
            }
            Err(e) => {
                self.message = Some(format!("Error: {e}"));
            }
        }
    }

    pub fn model_map_toggle_merge(&mut self, core: &Core) {
        let Some(mm) = self.model_map.as_mut() else {
            return;
        };
        let next = !mm.models_merge;
        match core.db().set_models_merge_enabled(next) {
            Ok(()) => {
                mm.models_merge = next;
                self.message = Some(if next {
                    "models-merge: ON (aggregated /v1/models)".into()
                } else {
                    "models-merge: OFF (first-success passthrough)".into()
                });
            }
            Err(e) => {
                self.message = Some(format!("Error: {e}"));
            }
        }
    }

    pub fn start_routes(&mut self, core: &Core) {
        let (existing_routes, model_override, target_protocol) = core
            .db()
            .get_routes()
            .ok()
            .flatten()
            // Default to "chat" when no route config exists yet; subsequent saves preserve DB value.
            .unwrap_or_else(|| (Vec::new(), None, "chat".to_string()));
        let route_models = core.db().get_route_models().unwrap_or_default();

        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        for rid in &existing_routes {
            if seen.insert(rid.clone()) {
                entries.push(RouteEntry {
                    provider_id: rid.clone(),
                    enabled: true,
                    model: route_models.get(rid).cloned().unwrap_or_default(),
                    available_models: Self::models_for_provider_choice(&self.model_probes, rid),
                });
            }
        }
        for p in &self.providers {
            if seen.insert(p.id.clone()) {
                entries.push(RouteEntry {
                    provider_id: p.id.clone(),
                    enabled: false,
                    model: route_models.get(&p.id).cloned().unwrap_or_default(),
                    available_models: Self::models_for_provider_choice(&self.model_probes, &p.id),
                });
            }
        }

        self.routes_edit = Some(RoutesState {
            entries,
            selected_row: 0,
            editing_model: false,
            target_protocol,
            model_override,
        });
        self.message = None;
    }

    pub fn routes_next(&mut self) {
        if let Some(r) = self.routes_edit.as_mut() {
            if !r.entries.is_empty() {
                r.selected_row = (r.selected_row + 1) % r.entries.len();
            }
        }
    }

    pub fn routes_prev(&mut self) {
        if let Some(r) = self.routes_edit.as_mut() {
            if !r.entries.is_empty() {
                r.selected_row = (r.selected_row + r.entries.len() - 1) % r.entries.len();
            }
        }
    }

    pub fn routes_toggle_selected(&mut self) {
        if let Some(r) = self.routes_edit.as_mut() {
            if let Some(entry) = r.entries.get_mut(r.selected_row) {
                entry.enabled = !entry.enabled;
            }
        }
    }

    pub fn routes_move_up(&mut self) {
        if let Some(r) = self.routes_edit.as_mut() {
            if r.selected_row > 0 && r.selected_row < r.entries.len() {
                r.entries.swap(r.selected_row, r.selected_row - 1);
                r.selected_row -= 1;
            }
        }
    }

    pub fn routes_move_down(&mut self) {
        if let Some(r) = self.routes_edit.as_mut() {
            if r.selected_row + 1 < r.entries.len() {
                r.entries.swap(r.selected_row, r.selected_row + 1);
                r.selected_row += 1;
            }
        }
    }

    pub fn routes_start_edit_model(&mut self) {
        if let Some(r) = self.routes_edit.as_mut() {
            if !r.entries.is_empty() {
                r.editing_model = true;
            }
        }
    }

    pub fn routes_finish_edit_model(&mut self) {
        if let Some(r) = self.routes_edit.as_mut() {
            r.editing_model = false;
        }
    }

    pub fn edit_char(&mut self, c: char) {
        if let Some(e) = self.edit.as_mut() {
            focused_field_mut(e).push(c);
        } else if let Some(a) = self.add.as_mut() {
            match a.field {
                AddField::Id => a.id.push(c),
                AddField::BaseUrl => a.base_url.push(c),
                AddField::Model => a.model.push(c),
                AddField::ApiKey => a.api_key.push(c),
                // Choice fields: Space steps forward, other keys are ignored.
                AddField::Protocol if c == ' ' => {
                    a.protocol = cycle_choice(&PROTOCOL_CHOICES, &a.protocol, true)
                }
                AddField::Official if c == ' ' => a.official = !a.official,
                AddField::Protocol | AddField::Official => {}
            }
        } else if let Some(config) = self.config_edit.as_mut() {
            match config.field {
                ConfigField::Secrets if c == ' ' => {
                    config.secrets = cycle_choice(&SECRETS_CHOICES, &config.secrets, true)
                }
                ConfigField::Api if c == ' ' => {
                    config.api = cycle_choice(&API_CHOICES, &config.api, true)
                }
                ConfigField::Secrets | ConfigField::Api => {}
                ConfigField::BaseUrl => config.base_url.push(c),
                ConfigField::Model => config.model.push(c),
                ConfigField::ApiKey => config.api_key.push(c),
            }
        } else if let Some(mm) = self.model_map.as_mut() {
            if let Some(ed) = mm.editing.as_mut() {
                match ed.field {
                    ModelMapField::Alias => ed.alias.push(c),
                    // Choice field: cycled via ←/→ so only valid providers (or "*") are selected.
                    ModelMapField::Provider => {}
                    ModelMapField::UpstreamModel => ed.upstream_model.push(c),
                }
            }
        } else if let Some(r) = self.routes_edit.as_mut() {
            if r.editing_model {
                if let Some(entry) = r.entries.get_mut(r.selected_row) {
                    entry.model.push(c);
                }
            }
        }
    }

    /// Left/Right on a choice field (or a model field with probed candidates)
    /// steps through its options (no-op on plain text fields). Space does the
    /// same going forward on choice fields.
    pub fn edit_cycle(&mut self, forward: bool) {
        if let Some(e) = self.edit.as_mut() {
            if e.field == EditField::Model {
                if let Some(next) = cycle_model_candidate(&e.available_models, &e.model, forward) {
                    e.model = next;
                }
            }
        } else if let Some(a) = self.add.as_mut() {
            match a.field {
                AddField::Model => {
                    if let Some(next) =
                        cycle_model_candidate(&a.available_models, &a.model, forward)
                    {
                        a.model = next;
                    }
                }
                AddField::Protocol => {
                    a.protocol = cycle_choice(&PROTOCOL_CHOICES, &a.protocol, forward)
                }
                AddField::Official => a.official = !a.official,
                _ => {}
            }
        } else if let Some(config) = self.config_edit.as_mut() {
            match config.field {
                ConfigField::Secrets => {
                    config.secrets = cycle_choice(&SECRETS_CHOICES, &config.secrets, forward)
                }
                ConfigField::Api => config.api = cycle_choice(&API_CHOICES, &config.api, forward),
                _ => {}
            }
        } else if let Some(mm) = self.model_map.as_mut() {
            let choices = mm.provider_choices.clone();
            if let Some(ed) = mm.editing.as_mut() {
                match ed.field {
                    ModelMapField::Provider if !choices.is_empty() => {
                        let n = choices.len();
                        let next = match choices.iter().position(|p| p == &ed.provider_id) {
                            Some(i) if forward => (i + 1) % n,
                            Some(i) => (i + n - 1) % n,
                            None if forward => 0,
                            None => n - 1,
                        };
                        ed.provider_id = choices[next].clone();
                        let pid = ed.provider_id.clone();
                        // Refresh candidate models for the newly selected provider.
                        ed.available_models =
                            Self::models_for_provider_choice(&self.model_probes, &pid);
                    }
                    ModelMapField::UpstreamModel => {
                        if let Some(next) =
                            cycle_model_candidate(&ed.available_models, &ed.upstream_model, forward)
                        {
                            ed.upstream_model = next;
                        }
                    }
                    _ => {}
                }
            }
        } else if let Some(r) = self.routes_edit.as_mut() {
            if r.editing_model {
                if let Some(entry) = r.entries.get_mut(r.selected_row) {
                    if let Some(next) =
                        cycle_model_candidate(&entry.available_models, &entry.model, forward)
                    {
                        entry.model = next;
                    }
                }
            }
        }
    }

    pub fn edit_backspace(&mut self) {
        if let Some(e) = self.edit.as_mut() {
            focused_field_mut(e).pop();
        } else if let Some(a) = self.add.as_mut() {
            match a.field {
                AddField::Id => {
                    a.id.pop();
                }
                AddField::BaseUrl => {
                    a.base_url.pop();
                }
                AddField::Model => {
                    a.model.pop();
                }
                AddField::ApiKey => {
                    a.api_key.pop();
                }
                // Choice fields: Backspace resets to the default option.
                AddField::Protocol => a.protocol.clear(),
                AddField::Official => {}
            }
        } else if let Some(config) = self.config_edit.as_mut() {
            match config.field {
                // Secrets always holds a valid backend; nothing to erase.
                ConfigField::Secrets => {}
                ConfigField::BaseUrl => {
                    config.base_url.pop();
                }
                ConfigField::Model => {
                    config.model.pop();
                }
                ConfigField::Api => config.api = DEFAULT_API.to_string(),
                ConfigField::ApiKey => {
                    config.api_key.pop();
                }
            }
        } else if let Some(mm) = self.model_map.as_mut() {
            if let Some(ed) = mm.editing.as_mut() {
                match ed.field {
                    ModelMapField::Alias => {
                        ed.alias.pop();
                    }
                    // Choice field: disallow Backspace so provider_id stays a valid choice.
                    ModelMapField::Provider => {}
                    ModelMapField::UpstreamModel => {
                        ed.upstream_model.pop();
                    }
                }
            }
        } else if let Some(r) = self.routes_edit.as_mut() {
            if r.editing_model {
                if let Some(entry) = r.entries.get_mut(r.selected_row) {
                    entry.model.pop();
                }
            }
        }
    }

    pub fn edit_next_field(&mut self) {
        if let Some(e) = self.edit.as_mut() {
            e.field = match e.field {
                EditField::BaseUrl => EditField::Model,
                EditField::Model => EditField::ApiKey,
                EditField::ApiKey => EditField::BaseUrl,
            };
        } else if let Some(a) = self.add.as_mut() {
            a.field = match a.field {
                AddField::Id => AddField::BaseUrl,
                AddField::BaseUrl => AddField::Model,
                AddField::Model => AddField::ApiKey,
                AddField::ApiKey => AddField::Protocol,
                AddField::Protocol => AddField::Official,
                AddField::Official => AddField::Id,
            };
        } else if let Some(config) = self.config_edit.as_mut() {
            config.field = match config.field {
                ConfigField::Secrets => ConfigField::BaseUrl,
                ConfigField::BaseUrl => ConfigField::Model,
                ConfigField::Model => ConfigField::Api,
                ConfigField::Api => ConfigField::ApiKey,
                ConfigField::ApiKey => ConfigField::Secrets,
            };
        } else if let Some(mm) = self.model_map.as_mut() {
            if let Some(ed) = mm.editing.as_mut() {
                ed.field = match ed.field {
                    ModelMapField::Alias => ModelMapField::Provider,
                    ModelMapField::Provider => ModelMapField::UpstreamModel,
                    ModelMapField::UpstreamModel => ModelMapField::Alias,
                };
            }
        }
    }

    pub fn edit_prev_field(&mut self) {
        if let Some(e) = self.edit.as_mut() {
            e.field = match e.field {
                EditField::BaseUrl => EditField::ApiKey,
                EditField::Model => EditField::BaseUrl,
                EditField::ApiKey => EditField::Model,
            };
        } else if let Some(a) = self.add.as_mut() {
            a.field = match a.field {
                AddField::Id => AddField::Official,
                AddField::BaseUrl => AddField::Id,
                AddField::Model => AddField::BaseUrl,
                AddField::ApiKey => AddField::Model,
                AddField::Protocol => AddField::ApiKey,
                AddField::Official => AddField::Protocol,
            };
        } else if let Some(config) = self.config_edit.as_mut() {
            config.field = match config.field {
                ConfigField::Secrets => ConfigField::ApiKey,
                ConfigField::BaseUrl => ConfigField::Secrets,
                ConfigField::Model => ConfigField::BaseUrl,
                ConfigField::Api => ConfigField::Model,
                ConfigField::ApiKey => ConfigField::Api,
            };
        } else if let Some(mm) = self.model_map.as_mut() {
            if let Some(ed) = mm.editing.as_mut() {
                ed.field = match ed.field {
                    ModelMapField::Alias => ModelMapField::UpstreamModel,
                    ModelMapField::Provider => ModelMapField::Alias,
                    ModelMapField::UpstreamModel => ModelMapField::Provider,
                };
            }
        }
    }

    pub fn cancel_edit(&mut self) {
        if let Some(mm) = self.model_map.as_mut() {
            if mm.editing.is_some() {
                mm.editing = None;
                return;
            }
        }
        if let Some(r) = self.routes_edit.as_mut() {
            if r.editing_model {
                r.editing_model = false;
                return;
            }
        }
        self.edit = None;
        self.add = None;
        self.config_edit = None;
        self.model_map = None;
        self.routes_edit = None;
    }

    pub fn submit_input(&mut self, core: &Core) {
        if self.edit.is_some() {
            self.submit_edit(core);
        } else if self.add.is_some() {
            self.submit_add(core);
        } else if self.config_edit.is_some() {
            self.submit_config(core);
        } else if self.model_map.is_some() {
            self.submit_model_map(core);
        } else if self.routes_edit.is_some() {
            self.submit_routes(core);
        }
    }

    fn submit_routes(&mut self, core: &Core) {
        let Some(r) = self.routes_edit.take() else {
            return;
        };
        let enabled: Vec<String> = r
            .entries
            .iter()
            .filter(|e| e.enabled)
            .map(|e| e.provider_id.clone())
            .collect();
        let mut route_models = std::collections::BTreeMap::new();
        for e in &r.entries {
            let m = e.model.trim();
            if e.enabled && !m.is_empty() {
                route_models.insert(e.provider_id.clone(), m.to_string());
            }
        }
        let res = if enabled.is_empty() {
            core.db()
                .clear_routes()
                .map(|()| "Cleared proxy routes".to_string())
        } else {
            core.db()
                .set_routes_with_models(
                    &enabled,
                    r.model_override.as_deref(),
                    &r.target_protocol,
                    &route_models,
                )
                .map(|()| {
                    format!(
                        "Saved {} routes ({} with fixed models)",
                        enabled.len(),
                        route_models.len()
                    )
                })
        };
        match res {
            Ok(msg) => self.message = Some(msg),
            Err(e) => {
                self.message = Some(format!("Error: {e}"));
                self.routes_edit = Some(r);
            }
        }
    }

    fn submit_model_map(&mut self, core: &Core) {
        let Some(ed) = self.model_map.as_mut().and_then(|mm| mm.editing.take()) else {
            return;
        };
        let alias = ed.alias.trim().to_string();
        let provider = ed.provider_id.trim().to_string();
        let upstream = ed.upstream_model.trim().to_string();
        if alias.is_empty() || provider.is_empty() || upstream.is_empty() {
            self.message =
                Some("Error: alias, provider, and upstream model are all required".into());
            if let Some(mm) = self.model_map.as_mut() {
                mm.editing = Some(ed);
            }
            return;
        }
        match core
            .db()
            .set_model_mappings_batch(&alias, &[(provider.clone(), upstream.clone())])
        {
            Ok(()) => {
                let (rows, models_merge) = Self::load_model_map_rows(core);
                if let Some(mm) = self.model_map.as_mut() {
                    mm.selected_row = rows
                        .iter()
                        .position(|(a, p, _)| a == &alias && p == &provider)
                        .unwrap_or(0);
                    mm.rows = rows;
                    mm.models_merge = models_merge;
                }
                self.message = Some(format!("Saved mapping: {alias} -> {provider}={upstream}"));
            }
            Err(e) => {
                self.message = Some(format!("Error: {e}"));
                if let Some(mm) = self.model_map.as_mut() {
                    mm.editing = Some(ed);
                }
            }
        }
    }

    fn submit_add(&mut self, core: &Core) {
        let Some(add) = self.add.take() else {
            return;
        };
        let protocol = if add.protocol.trim().is_empty() {
            None
        } else {
            match add.protocol.trim().parse::<ProviderProtocol>() {
                Ok(protocol) => Some(protocol),
                Err(error) => {
                    self.message = Some(format!("Error: {error}"));
                    self.add = Some(add);
                    return;
                }
            }
        };
        let requested_url = Some(add.base_url.as_str()).filter(|u| !u.trim().is_empty());
        // An empty URL with no global default would silently create an
        // official-login provider; require that to be an explicit choice.
        if !add.official && core.effective_base_url(requested_url, false).is_none() {
            self.message = Some("Error: Base URL required (or tick Official)".into());
            self.add = Some(add);
            return;
        }
        let extra = if add.model.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!({ "model": add.model.trim() })
        };
        let input = AddProviderInput {
            id: add.id.trim().to_string(),
            tool: self.tool,
            base_url: requested_url.map(|u| u.trim().to_string()),
            api_key: Some(add.api_key.trim().to_string()).filter(|k| !k.is_empty()),
            protocol,
            extra,
            official: add.official,
            overwrite: false,
        };
        match core.add_provider_from_input(input) {
            Ok(provider) => {
                self.message = Some(format!("Added {}", provider.id));
                self.reload(core);
                if let Some(pos) = self.providers.iter().position(|p| p.id == provider.id) {
                    self.selected = pos;
                }
            }
            Err(error) => {
                // Keep the form (and what was typed) so the user can fix it.
                self.message = Some(format!("Error: {error}"));
                self.add = Some(add);
            }
        }
    }

    fn submit_config(&mut self, core: &Core) {
        let Some(config) = self.config_edit.take() else {
            return;
        };
        let requested_backend = match config.secrets.trim().parse::<SecretsBackend>() {
            Ok(backend) => backend,
            Err(error) => {
                self.message = Some(format!("Error: {error}"));
                self.config_edit = Some(config);
                return;
            }
        };
        if requested_backend != config.active_secrets && !config.api_key.trim().is_empty() {
            self.message = Some(
                "Error: backend change pending; save, restart xfade, then set the API key".into(),
            );
            self.config_edit = Some(config);
            return;
        }
        let result = core.update_config(
            Some(config.secrets.trim()),
            Some(config.base_url.trim()),
            Some(config.model.trim()),
            Some(config.api.trim()),
        );
        if let Err(error) = result {
            self.message = Some(format!("Error: {error}"));
            self.config_edit = Some(config);
            return;
        }
        if !config.api_key.trim().is_empty() {
            if let Err(error) = core.set_global_api_key(config.api_key.trim()) {
                self.message = Some(format!("Settings saved, but API key failed: {error}"));
                return;
            }
        }
        self.message = Some(if requested_backend != config.active_secrets {
            "Saved global defaults; secrets backend changes after restart".into()
        } else {
            "Saved global defaults".into()
        });
    }

    pub fn request_delete(&mut self) {
        let Some((id, is_active)) = self
            .selected_provider()
            .map(|provider| (provider.id.clone(), provider.is_active))
        else {
            return;
        };
        if is_active {
            self.message = Some("Error: the active provider cannot be removed".into());
            return;
        }
        self.delete_confirm = Some(id.clone());
        self.message = Some(format!("Remove {id}? [y/N]"));
    }

    pub fn confirm_delete(&mut self, core: &Core, confirmed: bool) {
        let Some(id) = self.delete_confirm.take() else {
            return;
        };
        if confirmed {
            self.message = Some(match core.remove(self.tool, &id) {
                Ok(()) => {
                    // Drop stale probe state so a re-added id doesn't show an
                    // old latency badge.
                    self.probes.remove(&id);
                    self.probing.remove(&id);
                    self.model_probes.remove(&id);
                    self.model_probing.remove(&id);
                    format!("Removed {id}")
                }
                Err(error) => format!("Error: {error}"),
            });
            self.reload(core);
        } else {
            self.message = Some("Removal cancelled".into());
        }
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
            let direct = core.is_direct(self.tool).unwrap_or(false);
            if let Err(err) = core.use_provider(self.tool, &e.provider_id, direct) {
                self.message = Some(format!("Saved, but re-apply failed: {err}"));
            }
        }
        self.last_trust_refresh = Instant::now() - std::time::Duration::from_secs(10);
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

pub use xfade_core::service::{probe_latency, probe_target};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use xfade_core::store::secrets::FileStore;

    fn test_core(dir: &tempfile::TempDir) -> Core {
        std::env::set_var("XFADE_TEST_SKIP_SPAWN", "1");
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
        core.use_provider(ToolKind::ClaudeCode, "b", true).unwrap();
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
    fn prev_tool_cycles_backwards() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        let start = app.tool;
        app.next_tool(&core);
        assert_ne!(app.tool, start);
        app.prev_tool(&core);
        assert_eq!(app.tool, start);
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
        core.use_provider(ToolKind::Codex, "active-cx", true)
            .unwrap();
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
    fn open_animates_from_center_with_scheduled_intro() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(
            &core,
            ToolKind::ClaudeCode,
            "glm",
            Some("http://gw.example:3000"),
        );
        core.use_provider(ToolKind::ClaudeCode, "glm", true)
            .unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        // The knob starts centered, not snapped to the cloud target, and the
        // intro fade is scheduled to run.
        assert!(
            (app.fader_pos - 0.5).abs() < 1e-6,
            "open must start centered, got {}",
            app.fader_pos
        );
        assert!(app.is_animating(), "open must animate");
        assert!(app.intro_until.is_some(), "intro must be scheduled");
        // Re-pin the deadline so a stalled test runner (> INTRO_DURATION
        // between new() and this line) can't clamp progress to 1.0.
        app.intro_until = Some(Instant::now() + INTRO_DURATION);
        assert!(
            app.intro_progress() < 1.0,
            "intro must not be complete at open"
        );
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
        core.use_provider(ToolKind::ClaudeCode, "cloudy", true)
            .unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.intro_until = None; // isolate the switch slide from the open intro
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
        // is_animating also covers the (time-based) switch flash; clear it so
        // the final assert reflects the fader alone.
        app.flash_until = None;
        assert!(!app.is_animating());
    }

    #[test]
    fn is_animating_includes_active_flash() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        add(&core, ToolKind::ClaudeCode, "b", Some("http://y"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.intro_until = None;
        app.next();
        app.switch_selected(&core);
        assert!(app.flash_until.is_some(), "switch must set a flash");
        // Simulate the fader having settled while the flash is still active:
        // the frame loop must keep repainting so the flash actually clears.
        app.fader_pos = app.fader_target();
        assert!(
            app.is_animating(),
            "an active flash must keep is_animating true"
        );
    }

    #[test]
    fn user_interaction_overrides_slow_intro_easing() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(
            &core,
            ToolKind::ClaudeCode,
            "glm",
            Some("http://gw.example:3000"),
        );
        core.use_provider(ToolKind::ClaudeCode, "glm", true)
            .unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        assert!(app.intro_until.is_some(), "intro must be running");
        // Before interaction: one tick advances the knob by the slow factor.
        app.fader_pos = 0.5;
        app.tick();
        let slow_step = app.fader_pos;
        // After interaction: the same tick must advance faster, even while
        // the intro window is still open.
        app.fader_pos = 0.5;
        app.user_interacted = true;
        app.tick();
        assert!(
            app.fader_pos > slow_step,
            "interacted tick must outpace the intro glide: {} vs {slow_step}",
            app.fader_pos
        );
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
        core.use_provider(ToolKind::ClaudeCode, "a", true).unwrap();
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
    fn submit_add_uses_shared_global_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.update_config(
            None,
            Some("https://global.example"),
            Some("global-model"),
            None,
        )
        .unwrap();
        core.set_global_api_key("sk-global").unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);

        app.start_add(&core);
        {
            let add = app.add.as_mut().unwrap();
            add.id = "shared".into();
            // Blank values exercise the shared Core defaults rather than
            // merely persisting the values prefilled by start_add.
            add.base_url.clear();
            add.model.clear();
        }
        app.submit_input(&core);

        let provider = app
            .providers
            .iter()
            .find(|provider| provider.id == "shared")
            .unwrap();
        assert_eq!(provider.base_url.as_deref(), Some("https://global.example"));
        assert_eq!(provider.extra["model"], "global-model");
        assert_eq!(core.secrets().get(&provider.key_ref).unwrap(), "sk-global");
    }

    #[test]
    fn start_add_leaves_fields_blank_and_keeps_global_as_hints() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.update_config(None, Some("https://g.example"), Some("g-model"), None)
            .unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_add(&core);
        let add = app.add.as_ref().unwrap();
        assert!(add.base_url.is_empty() && add.model.is_empty());
        assert_eq!(add.global_base_url.as_deref(), Some("https://g.example"));
        assert_eq!(add.global_model.as_deref(), Some("g-model"));
    }

    #[test]
    fn submit_add_blank_url_without_global_requires_official() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_add(&core);
        app.add.as_mut().unwrap().id = "x".into();
        app.submit_input(&core);
        assert!(app.is_adding(), "form must stay open so input isn't lost");
        assert!(app
            .message
            .as_deref()
            .unwrap()
            .contains("Base URL required"));
        assert!(core.list(Some(ToolKind::ClaudeCode)).unwrap().is_empty());

        app.add.as_mut().unwrap().official = true;
        app.submit_input(&core);
        assert!(!app.is_adding());
        assert!(app.providers[0].is_official());
    }

    #[test]
    fn submit_add_duplicate_id_keeps_existing_and_form() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_add(&core);
        {
            let add = app.add.as_mut().unwrap();
            add.id = "a".into();
            add.base_url = "http://other".into();
            add.api_key = "sk-new".into();
        }
        app.submit_input(&core);
        assert!(app.is_adding());
        assert!(app.message.as_deref().unwrap().contains("already exists"));
        assert_eq!(app.providers[0].base_url.as_deref(), Some("http://x"));
        assert_eq!(core.secrets().get("xfade/claude/a").unwrap(), "sk-test");
    }

    #[test]
    fn submit_add_selects_the_new_provider() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "a", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_add(&core);
        {
            let add = app.add.as_mut().unwrap();
            add.id = "z".into();
            add.base_url = "http://z".into();
            add.api_key = "sk".into();
        }
        app.submit_input(&core);
        assert_eq!(app.selected_provider().map(|p| p.id.as_str()), Some("z"));
    }

    #[test]
    fn pending_backend_switch_refuses_global_key_write() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        // test_core injects a store, so "active" falls back to the config value
        // (keyring). Asking for a different backend therefore models the
        // pending-switch state: a key must not be written to the old store.
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_config(&core);
        let active = app.config_edit.as_ref().unwrap().active_secrets;
        assert_eq!(active, SecretsBackend::Keyring);
        {
            let c = app.config_edit.as_mut().unwrap();
            c.secrets = "file".into();
            c.api_key = "sk-would-go-to-old-store".into();
        }
        app.submit_input(&core);
        assert!(app.is_configuring(), "form must stay open");
        assert!(app.message.as_deref().unwrap().contains("backend change"));
        assert!(core.global_api_key().is_none());
        assert_eq!(core.config().secrets, SecretsBackend::Keyring);
    }

    #[test]
    fn delete_clears_stale_probe_state() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "gone", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.probes.insert("gone".into(), ProbeResult::Ms(5));
        app.request_delete();
        app.confirm_delete(&core, true);
        assert!(!app.probes.contains_key("gone"));
    }

    #[test]
    fn config_choice_fields_cycle_instead_of_accepting_typed_text() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_config(&core);

        // Secrets: typed text is ignored; Space/Right/Left cycle and wrap.
        app.edit_char('x');
        assert_eq!(app.config_edit.as_ref().unwrap().secrets, "keyring");
        app.edit_char(' ');
        assert_eq!(app.config_edit.as_ref().unwrap().secrets, "file");
        app.edit_cycle(true);
        assert_eq!(app.config_edit.as_ref().unwrap().secrets, "keyring");
        app.edit_cycle(false);
        assert_eq!(app.config_edit.as_ref().unwrap().secrets, "file");
        app.edit_backspace();
        assert_eq!(app.config_edit.as_ref().unwrap().secrets, "file");

        // API: openai (preselected) <-> anthropic; Backspace resets.
        app.edit_next_field();
        app.edit_next_field();
        app.edit_next_field();
        assert_eq!(app.config_edit.as_ref().unwrap().field, ConfigField::Api);
        assert_eq!(app.config_edit.as_ref().unwrap().api, "openai-completions");
        app.edit_cycle(true);
        assert_eq!(app.config_edit.as_ref().unwrap().api, "anthropic-messages");
        app.edit_cycle(true);
        assert_eq!(app.config_edit.as_ref().unwrap().api, "openai-completions");
        app.edit_cycle(false);
        assert_eq!(app.config_edit.as_ref().unwrap().api, "anthropic-messages");
        app.edit_backspace();
        assert_eq!(app.config_edit.as_ref().unwrap().api, "openai-completions");
    }

    #[test]
    fn add_protocol_and_official_are_choices() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_add(&core);
        for _ in 0..4 {
            app.edit_next_field();
        }
        assert_eq!(app.add.as_ref().unwrap().field, AddField::Protocol);
        app.edit_char('z'); // ignored
        assert_eq!(app.add.as_ref().unwrap().protocol, "");
        app.edit_cycle(true);
        assert_eq!(app.add.as_ref().unwrap().protocol, "chat");
        app.edit_char(' ');
        assert_eq!(app.add.as_ref().unwrap().protocol, "messages");
        app.edit_cycle(false);
        app.edit_cycle(false);
        assert_eq!(app.add.as_ref().unwrap().protocol, "");
        app.edit_cycle(false);
        assert_eq!(app.add.as_ref().unwrap().protocol, "responses");
        app.edit_backspace();
        assert_eq!(app.add.as_ref().unwrap().protocol, "");

        app.edit_next_field();
        assert_eq!(app.add.as_ref().unwrap().field, AddField::Official);
        app.edit_cycle(true);
        assert!(app.add.as_ref().unwrap().official);
        app.edit_cycle(false);
        assert!(!app.add.as_ref().unwrap().official);
    }

    #[test]
    fn cycle_choice_recovers_from_unknown_value() {
        // A hand-edited config.json value outside the list must not trap the
        // user: stepping lands on a valid option in either direction.
        assert_eq!(
            cycle_choice(&API_CHOICES, "weird", true),
            "openai-completions"
        );
        assert_eq!(
            cycle_choice(&API_CHOICES, "weird", false),
            "anthropic-messages"
        );
    }

    #[test]
    fn config_saves_chosen_secrets_and_api() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_config(&core);
        app.edit_cycle(true); // secrets -> file
        for _ in 0..3 {
            app.edit_next_field();
        }
        assert_eq!(app.config_edit.as_ref().unwrap().api, "openai-completions");
        app.edit_cycle(true); // api -> anthropic-messages
        app.submit_input(&core);
        let cfg = core.config();
        assert_eq!(cfg.secrets, SecretsBackend::File);
        assert_eq!(cfg.api.as_deref(), Some("anthropic-messages"));
        assert!(app.message.as_deref().unwrap().contains("restart"));
    }

    #[test]
    fn submit_global_config_updates_cli_visible_config() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.start_config(&core);
        {
            let config = app.config_edit.as_mut().unwrap();
            config.base_url = "https://gateway.example".into();
            config.model = "model-x".into();
            config.api = "anthropic-messages".into();
            config.api_key = "sk-shared".into();
        }
        app.submit_input(&core);

        let config = core.config();
        assert_eq!(config.base_url.as_deref(), Some("https://gateway.example"));
        assert_eq!(config.model.as_deref(), Some("model-x"));
        assert_eq!(config.api.as_deref(), Some("anthropic-messages"));
        assert_eq!(core.global_api_key().as_deref(), Some("sk-shared"));
    }

    #[test]
    fn delete_requires_confirmation_and_removes_non_active_provider() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "unused", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);

        app.request_delete();
        assert_eq!(app.delete_confirm.as_deref(), Some("unused"));
        assert!(core
            .list(Some(ToolKind::ClaudeCode))
            .unwrap()
            .iter()
            .any(|provider| provider.id == "unused"));
        app.confirm_delete(&core, true);
        assert!(app.providers.is_empty());
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

    #[test]
    fn cycle_model_candidate_wraps_and_handles_empty() {
        assert_eq!(cycle_model_candidate(&[], "x", true), None);
        let models = vec!["m1".into(), "m2".into(), "m3".into()];
        assert_eq!(cycle_model_candidate(&models, "", true), Some("m1".into()));
        assert_eq!(cycle_model_candidate(&models, "", false), Some("m3".into()));
        assert_eq!(
            cycle_model_candidate(&models, "m1", true),
            Some("m2".into())
        );
        assert_eq!(
            cycle_model_candidate(&models, "m1", false),
            Some("m3".into())
        );
    }

    #[test]
    fn start_edit_and_cycle_use_probed_models_and_tool_switch_clears_them() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into())),
            Some("sk"),
        )
        .unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.model_probes.insert(
            "glm".into(),
            ModelProbeResult::Models(vec!["glm-4".into(), "glm-4-plus".into()]),
        );
        app.model_probing.insert("glm".into());
        assert!(app.is_animating());

        app.start_edit();
        app.edit_next_field(); // -> EditField::Model
        app.edit_cycle(true);
        assert_eq!(app.edit.as_ref().unwrap().model, "glm-4");
        app.edit_cycle(true);
        assert_eq!(app.edit.as_ref().unwrap().model, "glm-4-plus");
        app.cancel_edit();

        app.next_tool(&core);
        assert!(app.model_probes.is_empty());
        assert!(app.model_probing.is_empty());
    }

    #[test]
    fn model_map_add_edit_delete_and_toggle_merge() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into())),
            Some("sk"),
        )
        .unwrap();
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.model_probes.insert(
            "glm".into(),
            ModelProbeResult::Models(vec!["glm-4".into(), "glm-4-plus".into()]),
        );

        app.start_model_map(&core);
        assert!(app.is_model_mapping());
        assert!(!app.model_map.as_ref().unwrap().models_merge);

        app.model_map_toggle_merge(&core);
        assert!(app.model_map.as_ref().unwrap().models_merge);
        assert!(core.db().models_merge_enabled().unwrap());

        app.model_map_start_add();
        for c in "sonnet".chars() {
            app.edit_char(c);
        }
        app.edit_next_field(); // Provider (defaults to "glm")
        app.edit_next_field(); // UpstreamModel
        app.edit_cycle(true); // picks "glm-4"
        app.submit_input(&core);

        let mm = app.model_map.as_ref().unwrap();
        assert!(mm.editing.is_none());
        assert_eq!(
            mm.rows,
            vec![("sonnet".into(), "glm".into(), "glm-4".into())]
        );
        assert_eq!(
            core.db()
                .get_model_mappings()
                .unwrap()
                .get("sonnet")
                .and_then(|m| m.get("glm"))
                .map(String::as_str),
            Some("glm-4")
        );

        app.model_map_delete(&core);
        assert!(app.model_map.as_ref().unwrap().rows.is_empty());
        app.cancel_edit();
        assert!(!app.is_model_mapping());
    }

    #[test]
    fn routes_toggle_reorder_edit_model_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        core.add_provider(
            Provider::new("glm", ToolKind::ClaudeCode, Some("http://gw:3000".into())),
            Some("sk1"),
        )
        .unwrap();
        core.add_provider(
            Provider::new("ds", ToolKind::ClaudeCode, Some("http://ds:3000".into())),
            Some("sk2"),
        )
        .unwrap();

        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.model_probes.insert(
            "ds".into(),
            ModelProbeResult::Models(vec!["deepseek-chat".into(), "deepseek-reasoner".into()]),
        );

        app.start_routes(&core);
        assert!(app.is_routing());

        // Enable first route ("ds" or "glm" depending on list order)
        let first_id = app.routes_edit.as_ref().unwrap().entries[0]
            .provider_id
            .clone();
        app.routes_toggle_selected();
        app.routes_next();
        let second_id = app.routes_edit.as_ref().unwrap().entries[1]
            .provider_id
            .clone();
        app.routes_toggle_selected();

        // Swap second into first place
        app.routes_move_up();
        assert_eq!(
            app.routes_edit.as_ref().unwrap().entries[0].provider_id,
            second_id
        );

        // Set model on whichever is "ds"
        if second_id == "ds" {
            app.routes_start_edit_model();
            app.edit_cycle(true);
            app.routes_finish_edit_model();
        } else {
            app.routes_next();
            app.routes_start_edit_model();
            app.edit_cycle(true);
            app.routes_finish_edit_model();
        }

        app.submit_input(&core);
        assert!(!app.is_routing());

        let (routes, _, _) = core.db().get_routes().unwrap().unwrap();
        assert_eq!(routes, vec![second_id, first_id]);
        let rm = core.db().get_route_models().unwrap();
        assert_eq!(rm.get("ds").map(String::as_str), Some("deepseek-chat"));
    }

    #[test]
    fn delete_clears_stale_model_probe_state() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "gone", Some("http://x"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.model_probes
            .insert("gone".into(), ModelProbeResult::Models(vec!["m1".into()]));
        app.model_probing.insert("gone".into());
        app.request_delete();
        app.confirm_delete(&core, true);
        assert!(!app.model_probes.contains_key("gone"));
        assert!(!app.model_probing.contains("gone"));
    }

    #[test]
    fn model_map_provider_field_is_cycle_only_and_wildcard_models_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir);
        add(&core, ToolKind::ClaudeCode, "glm", Some("http://gw:3000"));
        let mut app = App::new(ToolKind::ClaudeCode, &core);
        app.model_probes.insert(
            "glm".into(),
            ModelProbeResult::Models(vec!["z-model".into(), "a-model".into()]),
        );

        let sorted = App::models_for_provider_choice(&app.model_probes, "*");
        assert_eq!(sorted, vec!["a-model", "z-model"]);

        app.start_model_map(&core);
        app.model_map_start_add();
        app.edit_next_field(); // move to Provider
        let initial = app
            .model_map
            .as_ref()
            .unwrap()
            .editing
            .as_ref()
            .unwrap()
            .provider_id
            .clone();
        app.edit_char('x');
        app.edit_backspace();
        assert_eq!(
            app.model_map
                .as_ref()
                .unwrap()
                .editing
                .as_ref()
                .unwrap()
                .provider_id,
            initial
        );
    }
}
