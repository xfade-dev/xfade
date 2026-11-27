use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::models::{Provider, ToolKind};

/// Capability tier used for matching client aliases to discovered upstream models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Flagship,
    Balanced,
    Fast,
    Aux,
}

impl ModelTier {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Flagship => "flagship",
            Self::Balanced => "balanced",
            Self::Fast => "fast",
            Self::Aux => "aux",
        }
    }
}

/// How a template rule's upstream model was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchSource {
    ProbeExact,
    ProbeTier,
    Fallback,
    Unmatched,
}

/// Mode for applying a template or switching a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApplyMode {
    #[default]
    Replace,
    Merge,
}

/// Single alias rule inside a [`ModelMapTemplate`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateAliasRule {
    pub client_alias: String,
    pub tier: ModelTier,
    #[serde(default)]
    pub preferred_patterns: Vec<String>,
    #[serde(default)]
    pub vendor_fallbacks: BTreeMap<String, String>,
}

/// Built-in or user-defined model mapping template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMapTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub client_tool: Option<ToolKind>,
    pub rules: Vec<TemplateAliasRule>,
}

/// Result of resolving a [`ModelMapTemplate`] against a target provider and optional
/// `/v1/models` probe catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedTemplateRule {
    pub client_alias: String,
    pub provider_id: String,
    pub upstream_model: String,
    pub tier: ModelTier,
    pub match_source: MatchSource,
}

fn fallbacks(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn rule(
    client_alias: &str,
    tier: ModelTier,
    preferred: &[&str],
    vendor_pairs: &[(&str, &str)],
) -> TemplateAliasRule {
    TemplateAliasRule {
        client_alias: client_alias.to_string(),
        tier,
        preferred_patterns: preferred.iter().map(|s| (*s).to_string()).collect(),
        vendor_fallbacks: fallbacks(vendor_pairs),
    }
}

/// Built-in templates shipped with `xfade`, merged with optional user templates
/// from `~/.config/xfade/model_map_templates.json`.
pub fn all_templates() -> Vec<ModelMapTemplate> {
    let mut templates = builtin_templates();
    if let Some(custom) = load_custom_templates() {
        for t in custom {
            if let Some(pos) = templates.iter().position(|existing| existing.id == t.id) {
                templates[pos] = t;
            } else {
                templates.push(t);
            }
        }
    }
    templates
}

pub fn find_template(id: &str) -> Option<ModelMapTemplate> {
    let needle = id.trim();
    all_templates().into_iter().find(|t| t.id == needle)
}

pub fn builtin_templates() -> Vec<ModelMapTemplate> {
    let opus_vendors: &[(&str, &str)] = &[
        ("anthropic", "claude-opus-4-1"),
        ("openrouter", "anthropic/claude-opus-4"),
        ("deepseek", "deepseek-reasoner"),
        ("glm", "glm-4.5"),
        ("kimi", "kimi-k2-thinking"),
        ("openai", "o3"),
        ("gemini", "gemini-2.5-pro"),
    ];
    let sonnet_vendors: &[(&str, &str)] = &[
        ("anthropic", "claude-sonnet-4-5"),
        ("openrouter", "anthropic/claude-sonnet-4"),
        ("deepseek", "deepseek-chat"),
        ("glm", "glm-4.5"),
        ("kimi", "kimi-k2-0711-preview"),
        ("openai", "gpt-5"),
        ("gemini", "gemini-2.5-pro"),
    ];
    let haiku_vendors: &[(&str, &str)] = &[
        ("anthropic", "claude-haiku-4-5"),
        ("openrouter", "anthropic/claude-haiku-4-5"),
        ("deepseek", "deepseek-chat"),
        ("glm", "glm-4.5-air"),
        ("kimi", "kimi-k2-turbo-preview"),
        ("openai", "gpt-5-mini"),
        ("gemini", "gemini-2.5-flash"),
    ];

    vec![
        ModelMapTemplate {
            id: "claude-code-standard".into(),
            name: "Claude Code Standard".into(),
            description: "Map Opus/Sonnet/Haiku aliases (full & short) to matched upstream models"
                .into(),
            client_tool: Some(ToolKind::ClaudeCode),
            rules: vec![
                rule(
                    "claude-opus-4",
                    ModelTier::Flagship,
                    &["claude-opus-4", "claude-opus"],
                    opus_vendors,
                ),
                rule(
                    "opus",
                    ModelTier::Flagship,
                    &["claude-opus-4", "claude-opus"],
                    opus_vendors,
                ),
                rule(
                    "claude-sonnet-4",
                    ModelTier::Balanced,
                    &["claude-sonnet-4", "claude-3-7-sonnet", "claude-sonnet"],
                    sonnet_vendors,
                ),
                rule(
                    "sonnet",
                    ModelTier::Balanced,
                    &["claude-sonnet-4", "claude-3-7-sonnet", "claude-sonnet"],
                    sonnet_vendors,
                ),
                rule(
                    "claude-haiku-4-5",
                    ModelTier::Fast,
                    &["claude-haiku-4-5", "claude-3-5-haiku", "claude-haiku"],
                    haiku_vendors,
                ),
                rule(
                    "haiku",
                    ModelTier::Fast,
                    &["claude-haiku-4-5", "claude-3-5-haiku", "claude-haiku"],
                    haiku_vendors,
                ),
            ],
        },
        ModelMapTemplate {
            id: "codex-standard".into(),
            name: "Codex / Responses Standard".into(),
            description: "Map OpenAI/Codex model aliases (gpt-5, gpt-5-mini, o3, o4-mini)".into(),
            client_tool: Some(ToolKind::Codex),
            rules: vec![
                rule(
                    "o3",
                    ModelTier::Flagship,
                    &["o3", "o3-pro"],
                    &[
                        ("openai", "o3"),
                        ("openrouter", "openai/o3"),
                        ("deepseek", "deepseek-reasoner"),
                        ("kimi", "kimi-k2-thinking"),
                        ("glm", "glm-4.5"),
                        ("anthropic", "claude-opus-4-1"),
                        ("gemini", "gemini-2.5-pro"),
                    ],
                ),
                rule(
                    "gpt-5",
                    ModelTier::Balanced,
                    &["gpt-5", "gpt-4.1", "gpt-4o"],
                    &[
                        ("openai", "gpt-5"),
                        ("openrouter", "openai/gpt-5"),
                        ("deepseek", "deepseek-chat"),
                        ("kimi", "kimi-k2-0711-preview"),
                        ("glm", "glm-4.5"),
                        ("anthropic", "claude-sonnet-4-5"),
                        ("gemini", "gemini-2.5-pro"),
                    ],
                ),
                rule(
                    "gpt-5-mini",
                    ModelTier::Fast,
                    &["gpt-5-mini", "gpt-4.1-mini", "gpt-4o-mini"],
                    &[
                        ("openai", "gpt-5-mini"),
                        ("openrouter", "openai/gpt-5-mini"),
                        ("deepseek", "deepseek-chat"),
                        ("kimi", "kimi-k2-turbo-preview"),
                        ("glm", "glm-4.5-air"),
                        ("anthropic", "claude-haiku-4-5"),
                        ("gemini", "gemini-2.5-flash"),
                    ],
                ),
                rule(
                    "o4-mini",
                    ModelTier::Fast,
                    &["o4-mini", "o3-mini"],
                    &[
                        ("openai", "o4-mini"),
                        ("openrouter", "openai/o4-mini"),
                        ("deepseek", "deepseek-reasoner"),
                        ("kimi", "kimi-k2-turbo-preview"),
                        ("glm", "glm-4.5-air"),
                        ("anthropic", "claude-haiku-4-5"),
                        ("gemini", "gemini-2.5-flash"),
                    ],
                ),
            ],
        },
        ModelMapTemplate {
            id: "agy-gemini".into(),
            name: "Antigravity / Gemini".into(),
            description: "Map Gemini Pro/Flash & auxiliary title-gen aliases".into(),
            client_tool: Some(ToolKind::Agy),
            rules: vec![
                rule(
                    "gemini-2.5-pro",
                    ModelTier::Flagship,
                    &["gemini-2.5-pro", "gemini-pro"],
                    &[
                        ("gemini", "gemini-2.5-pro"),
                        ("openrouter", "google/gemini-2.5-pro"),
                        ("anthropic", "claude-opus-4-1"),
                        ("openai", "o3"),
                        ("deepseek", "deepseek-reasoner"),
                    ],
                ),
                rule(
                    "gemini-2.5-flash",
                    ModelTier::Balanced,
                    &["gemini-2.5-flash", "gemini-3.8-flash", "gemini-flash"],
                    &[
                        ("gemini", "gemini-2.5-flash"),
                        ("openrouter", "google/gemini-2.5-flash"),
                        ("anthropic", "claude-sonnet-4-5"),
                        ("openai", "gpt-5"),
                        ("deepseek", "deepseek-chat"),
                    ],
                ),
                rule(
                    "gemini-3.1-flash-lite-preview",
                    ModelTier::Aux,
                    &[
                        "gemini-3.8-flash",
                        "gemini-2.5-flash-lite",
                        "gemini-2.5-flash",
                    ],
                    &[
                        ("gemini", "gemini-2.5-flash"),
                        ("openrouter", "google/gemini-2.5-flash-lite"),
                        ("anthropic", "claude-haiku-4-5"),
                        ("openai", "gpt-5-mini"),
                        ("deepseek", "deepseek-chat"),
                        ("glm", "glm-4.5-air"),
                        ("kimi", "kimi-k2-turbo-preview"),
                    ],
                ),
            ],
        },
        ModelMapTemplate {
            id: "cost-saver".into(),
            name: "Cost Saver (All Fast/Balanced)".into(),
            description:
                "Route flagship & standard client aliases to fast/low-cost upstream models".into(),
            client_tool: None,
            rules: vec![
                rule(
                    "claude-opus-4",
                    ModelTier::Fast,
                    &["claude-haiku", "deepseek-chat", "glm-4.5-air", "gpt-5-mini"],
                    haiku_vendors,
                ),
                rule(
                    "opus",
                    ModelTier::Fast,
                    &["claude-haiku", "deepseek-chat", "glm-4.5-air", "gpt-5-mini"],
                    haiku_vendors,
                ),
                rule(
                    "claude-sonnet-4",
                    ModelTier::Fast,
                    &["claude-haiku", "deepseek-chat", "glm-4.5-air", "gpt-5-mini"],
                    haiku_vendors,
                ),
                rule(
                    "sonnet",
                    ModelTier::Fast,
                    &["claude-haiku", "deepseek-chat", "glm-4.5-air", "gpt-5-mini"],
                    haiku_vendors,
                ),
                rule(
                    "claude-haiku-4-5",
                    ModelTier::Fast,
                    &["claude-haiku", "deepseek-chat", "glm-4.5-air", "gpt-5-mini"],
                    haiku_vendors,
                ),
                rule(
                    "haiku",
                    ModelTier::Fast,
                    &["claude-haiku", "deepseek-chat", "glm-4.5-air", "gpt-5-mini"],
                    haiku_vendors,
                ),
                rule(
                    "gpt-5",
                    ModelTier::Fast,
                    &["gpt-5-mini", "deepseek-chat", "glm-4.5-air"],
                    haiku_vendors,
                ),
            ],
        },
    ]
}

fn custom_templates_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    Some(base.join("xfade").join("model_map_templates.json"))
}

fn load_custom_templates() -> Option<Vec<ModelMapTemplate>> {
    let path = custom_templates_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Vec<ModelMapTemplate>>(&raw).ok()
}

/// Infer a canonical vendor key from provider id and optional base_url.
pub fn infer_vendor_key(provider_id: &str, base_url: Option<&str>) -> Option<&'static str> {
    let hay = format!(
        "{} {}",
        provider_id.to_ascii_lowercase(),
        base_url.unwrap_or("").to_ascii_lowercase()
    );
    if hay.contains("openrouter") {
        Some("openrouter")
    } else if hay.contains("deepseek") {
        Some("deepseek")
    } else if hay.contains("glm") || hay.contains("bigmodel") || hay.contains("zhipu") {
        Some("glm")
    } else if hay.contains("kimi") || hay.contains("moonshot") {
        Some("kimi")
    } else if hay.contains("anthropic") || hay.contains("claude") {
        Some("anthropic")
    } else if hay.contains("gemini") || hay.contains("generativelanguage") || hay.contains("google")
    {
        Some("gemini")
    } else if hay.contains("openai") || hay.contains("codex") {
        Some("openai")
    } else {
        None
    }
}

fn match_preferred_in_catalog(patterns: &[String], catalog: &[String]) -> Option<String> {
    if catalog.is_empty() || patterns.is_empty() {
        return None;
    }
    let mut best: Option<(u8, usize, &str)> = None;
    for pat in patterns {
        let p = pat.to_ascii_lowercase();
        for cand in catalog {
            let c = cand.to_ascii_lowercase();
            let tail = c.rsplit('/').next().unwrap_or(&c);
            let score = if c == p {
                Some(0u8)
            } else if tail == p {
                Some(1u8)
            } else if tail.starts_with(&p) {
                Some(2u8)
            } else if c.contains(&p) {
                Some(3u8)
            } else {
                None
            };
            if let Some(rank) = score {
                let candidate_key = (rank, cand.len(), cand.as_str());
                if best.is_none_or(|b| (candidate_key.0, candidate_key.1) < (b.0, b.1)) {
                    best = Some(candidate_key);
                }
            }
        }
        if best.is_some() {
            break;
        }
    }
    best.map(|(_, _, s)| s.to_string())
}

fn match_tier_in_catalog(tier: ModelTier, catalog: &[String]) -> Option<String> {
    if catalog.is_empty() {
        return None;
    }
    let is_light = |c: &str| {
        c.contains("mini")
            || c.contains("lite")
            || c.contains("air")
            || c.contains("nano")
            || c.contains("haiku")
            || c.contains("flash")
    };
    let keywords: &[&str] = match tier {
        ModelTier::Flagship => &["opus", "reasoner", "r1", "o3", "pro", "max", "thinking"],
        ModelTier::Balanced => &[
            "sonnet", "chat", "v3", "plus", "k2", "gpt-5", "gpt-4o", "4.5",
        ],
        ModelTier::Fast | ModelTier::Aux => &[
            "haiku",
            "flash-lite",
            "flash",
            "mini",
            "lite",
            "air",
            "nano",
            "turbo",
        ],
    };

    let mut best: Option<(usize, usize, &str)> = None;
    for (kw_idx, kw) in keywords.iter().enumerate() {
        for cand in catalog {
            let c = cand.to_ascii_lowercase();
            if !c.contains(kw) {
                continue;
            }
            if matches!(tier, ModelTier::Flagship) && is_light(&c) {
                continue;
            }
            let key = (kw_idx, cand.len(), cand.as_str());
            if best.is_none_or(|b| (key.0, key.1) < (b.0, b.1)) {
                best = Some(key);
            }
        }
    }
    best.map(|(_, _, s)| s.to_string())
}

/// Resolve every rule in `template` for `provider_id` using discovered upstream models
/// first, then vendor fallbacks.
pub fn resolve_template_for_provider(
    template: &ModelMapTemplate,
    provider_id: &str,
    provider_base_url: Option<&str>,
    discovered_models: &[String],
) -> Vec<ResolvedTemplateRule> {
    let vendor = infer_vendor_key(provider_id, provider_base_url);
    template
        .rules
        .iter()
        .map(|r| {
            if let Some(m) = match_preferred_in_catalog(&r.preferred_patterns, discovered_models) {
                ResolvedTemplateRule {
                    client_alias: r.client_alias.clone(),
                    provider_id: provider_id.to_string(),
                    upstream_model: m,
                    tier: r.tier,
                    match_source: MatchSource::ProbeExact,
                }
            } else if let Some(m) = match_tier_in_catalog(r.tier, discovered_models) {
                ResolvedTemplateRule {
                    client_alias: r.client_alias.clone(),
                    provider_id: provider_id.to_string(),
                    upstream_model: m,
                    tier: r.tier,
                    match_source: MatchSource::ProbeTier,
                }
            } else if let Some(m) = vendor.and_then(|v| r.vendor_fallbacks.get(v)) {
                ResolvedTemplateRule {
                    client_alias: r.client_alias.clone(),
                    provider_id: provider_id.to_string(),
                    upstream_model: m.clone(),
                    tier: r.tier,
                    match_source: MatchSource::Fallback,
                }
            } else {
                ResolvedTemplateRule {
                    client_alias: r.client_alias.clone(),
                    provider_id: provider_id.to_string(),
                    upstream_model: String::new(),
                    tier: r.tier,
                    match_source: MatchSource::Unmatched,
                }
            }
        })
        .collect()
}

/// Convenience helper when resolving against a registered [`Provider`].
pub fn resolve_template_with_provider(
    template: &ModelMapTemplate,
    provider: Option<&Provider>,
    provider_id: &str,
    discovered_models: &[String],
) -> Vec<ResolvedTemplateRule> {
    let base_url = provider.and_then(|p| p.base_url.as_deref());
    resolve_template_for_provider(template, provider_id, base_url, discovered_models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_template_prefers_probe_exact_over_tier_and_fallback() {
        let tpl = find_template("claude-code-standard").unwrap();
        let catalog = vec![
            "anthropic/claude-opus-4".to_string(),
            "anthropic/claude-sonnet-4".to_string(),
            "anthropic/claude-haiku-4-5".to_string(),
        ];
        let resolved = resolve_template_for_provider(
            &tpl,
            "openrouter",
            Some("https://openrouter.ai/api/v1"),
            &catalog,
        );
        assert_eq!(resolved.len(), 6);
        let sonnet = resolved
            .iter()
            .find(|r| r.client_alias == "sonnet")
            .unwrap();
        assert_eq!(sonnet.upstream_model, "anthropic/claude-sonnet-4");
        assert_eq!(sonnet.match_source, MatchSource::ProbeExact);
    }

    #[test]
    fn resolve_template_uses_probe_tier_for_heterogeneous_catalog() {
        let tpl = find_template("claude-code-standard").unwrap();
        let catalog = vec!["deepseek-chat".to_string(), "deepseek-reasoner".to_string()];
        let resolved =
            resolve_template_for_provider(&tpl, "ds", Some("https://api.deepseek.com"), &catalog);
        let opus = resolved.iter().find(|r| r.client_alias == "opus").unwrap();
        assert_eq!(opus.upstream_model, "deepseek-reasoner");
        assert_eq!(opus.match_source, MatchSource::ProbeTier);

        let sonnet = resolved
            .iter()
            .find(|r| r.client_alias == "sonnet")
            .unwrap();
        assert_eq!(sonnet.upstream_model, "deepseek-chat");
        assert_eq!(sonnet.match_source, MatchSource::ProbeTier);
    }

    #[test]
    fn resolve_template_falls_back_when_catalog_empty() {
        let tpl = find_template("claude-code-standard").unwrap();
        let resolved = resolve_template_for_provider(
            &tpl,
            "glm",
            Some("https://open.bigmodel.cn/api/anthropic"),
            &[],
        );
        let haiku = resolved.iter().find(|r| r.client_alias == "haiku").unwrap();
        assert_eq!(haiku.upstream_model, "glm-4.5-air");
        assert_eq!(haiku.match_source, MatchSource::Fallback);
    }
}
