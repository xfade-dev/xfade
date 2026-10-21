//! Smart Fader: intent classification, route planning, and tiered routing.
//!
//! Phase 2 of the V2 blueprint. The router is pluggable: a `DefaultRouter`
//! preserves the static failover order (smart fader off), while `SmartRouter`
//! classifies each prompt and re-orders routes across local/cloud tiers.
//! See `~/workspace/xfade-blueprint/BLUEPRINT.md` §Smart Fader.

pub mod classifier;
pub mod decision;

pub use classifier::{ClassifyCtx, Intent, IntentClassifier, KeywordClassifier};
pub use decision::{DefaultRouter, HealthSignal, RouteEntry, RoutePlan, Router, SmartRouter, Tier};
