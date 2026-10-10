//! Intent classification for the Smart Fader.
//!
//! Phase-one classifier is pure keyword rules: zero latency, zero cost,
//! fully unit-testable. The `IntentClassifier` trait leaves room for a
//! local small-model classifier in a later phase.

/// What kind of work does this prompt ask for?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// "Why does this fail" / traceback / panic triage.
    ExplainError,
    /// "Write a test for X".
    WriteTest,
    /// Lint / format / type-check sized asks.
    StaticCheck,
    /// Rename, extract function, small local edits.
    SmallRefactor,
    /// Cross-module restructuring, migrations, rewrites.
    BigRefactor,
    /// Design discussions, trade-offs, comparisons.
    Architecture,
    /// Nothing matched: keep static route order.
    Unknown,
}

/// Everything a classifier may look at for one request.
pub struct ClassifyCtx<'a> {
    /// Request endpoint: "chat" | "responses" | "messages".
    pub endpoint: &'a str,
    /// Model the client asked for, if any.
    pub model: Option<&'a str>,
    /// Text of the last user message (already extracted from the body).
    pub prompt: &'a str,
}

/// Pluggable intent classifier.
pub trait IntentClassifier: Send + Sync {
    fn classify(&self, ctx: &ClassifyCtx<'_>) -> Intent;
}

/// Pure keyword-rule classifier (phase one).
///
/// Ordered rules, first match wins. Ordering encodes two decisions:
/// - `ExplainError` requires a *triage verb* ("why", "explain", "traceback",
///   ...), not a bare "error" mention — "fix the lint errors" is a static
///   check and "refactor X so this error goes away" is a refactor.
/// - `BigRefactor` precedes `SmallRefactor` so "rewrite/restructure/migrate"
///   outrank the generic "refactor" keyword.
pub struct KeywordClassifier;

/// (intent, keywords) in precedence order; matched as case-insensitive
/// substrings of the last user message.
const RULES: &[(Intent, &[&str])] = &[
    (
        Intent::WriteTest,
        &[
            "write test", "write a test", "write tests", "unit test", "add test",
            "add a test", "test for", "tests for",
        ],
    ),
    (
        Intent::ExplainError,
        &[
            "why", "explain", "traceback", "panick", "stack trace", "what happened",
            "what's wrong", "whats wrong", "help me understand", "doesn't work",
            "does not work", "not working", "build fails", "build failed",
            "compile error", "failing",
        ],
    ),
    (
        Intent::StaticCheck,
        &[
            "lint", "clippy", "eslint", "ruff", "unused import", "type check",
            "typecheck", "format",
        ],
    ),
    (
        Intent::BigRefactor,
        &["restructure", "migrate", "rewrite", "overhaul", "large refactor", "big refactor"],
    ),
    (
        Intent::SmallRefactor,
        &["rename", "extract function", "extract this block", "refactor", "simplify", "inline", "clean up"],
    ),
    (
        Intent::Architecture,
        &[
            "design", "architecture", "trade-off", "tradeoff", "compare approaches",
            "should we use", "should i use", "pros and cons", "evaluate",
        ],
    ),
];

impl IntentClassifier for KeywordClassifier {
    fn classify(&self, ctx: &ClassifyCtx<'_>) -> Intent {
        let prompt = ctx.prompt.to_lowercase();
        for (intent, keywords) in RULES {
            if keywords.iter().any(|k| prompt.contains(k)) {
                return *intent;
            }
        }
        Intent::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(prompt: &str) -> Intent {
        let ctx = ClassifyCtx {
            endpoint: "chat",
            model: None,
            prompt,
        };
        KeywordClassifier.classify(&ctx)
    }

    #[test]
    fn error_triage_prompts() {
        assert_eq!(classify("why does this fail to compile?"), Intent::ExplainError);
        assert_eq!(classify("explain this traceback"), Intent::ExplainError);
        assert_eq!(classify("thread 'main' panicked, what happened"), Intent::ExplainError);
        assert_eq!(
            classify("the build fails with a weird error, help me understand"),
            Intent::ExplainError
        );
    }

    #[test]
    fn test_writing_prompts() {
        assert_eq!(classify("write a test for this function"), Intent::WriteTest);
        assert_eq!(classify("add unit tests for the parser"), Intent::WriteTest);
    }

    #[test]
    fn static_check_prompts() {
        assert_eq!(classify("run clippy and fix the warnings"), Intent::StaticCheck);
        assert_eq!(classify("fix the lint errors in main.rs"), Intent::StaticCheck);
        assert_eq!(classify("remove the unused imports"), Intent::StaticCheck);
    }

    #[test]
    fn small_refactor_prompts() {
        assert_eq!(classify("rename this variable to something clearer"), Intent::SmallRefactor);
        assert_eq!(classify("extract this block into a helper function"), Intent::SmallRefactor);
        assert_eq!(classify("refactor this function"), Intent::SmallRefactor);
    }

    #[test]
    fn big_refactor_prompts() {
        assert_eq!(
            classify("restructure the whole proxy module into layers"),
            Intent::BigRefactor
        );
        assert_eq!(classify("migrate the config layer to a new schema"), Intent::BigRefactor);
        assert_eq!(classify("rewrite the storage engine"), Intent::BigRefactor);
    }

    #[test]
    fn architecture_prompts() {
        assert_eq!(
            classify("design the API for the sync module"),
            Intent::Architecture
        );
        assert_eq!(
            classify("should we use redis or postgres for the queue? compare approaches"),
            Intent::Architecture
        );
    }

    #[test]
    fn unmatched_prompts_are_unknown() {
        assert_eq!(classify("hello, who are you?"), Intent::Unknown);
        assert_eq!(classify("summarize this document"), Intent::Unknown);
        assert_eq!(classify(""), Intent::Unknown);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(classify("EXPLAIN this error"), Intent::ExplainError);
        assert_eq!(classify("Write A TEST for parse"), Intent::WriteTest);
    }

    #[test]
    fn error_mention_without_triage_verb_is_not_error_intent() {
        // "refactor ... so this error goes away": the ask is a refactor;
        // a bare "error" mention is context, not a triage request. The
        // ExplainError bucket requires triage verbs (why/explain/...).
        assert_eq!(
            classify("refactor the parser so this error goes away"),
            Intent::SmallRefactor
        );
    }
}
