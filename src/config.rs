//! TOML config: defaults, per-activity and per-workspace tracking rules.

use std::collections::BTreeMap;

use anyhow::{Context as _, bail};
use serde::Deserialize;

/// If `key` is a regex-form mapping key `/source/`, return the inner `source`
/// (which may be empty). Ordinary literal keys return `None`. `/` is ASCII, so
/// byte checks and the slice are always on char boundaries.
fn regex_source(key: &str) -> Option<&str> {
    let b = key.as_bytes();
    (b.len() >= 2 && b[0] == b'/' && b[b.len() - 1] == b'/').then(|| &key[1..key.len() - 1])
}

/// Parsed and validated configuration.
///
/// Construct via [`Config::parse`] — the validation there is what `effective` relies on.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub activities: BTreeMap<String, ActivityRule>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, WorkspaceRule>,
    /// Compiled activity regex rules, paired with their map key, in sorted-key
    /// order (BTreeMap iteration order). Populated by `build_patterns`.
    #[serde(skip)]
    activity_patterns: Vec<(regex::Regex, String)>,
    /// Compiled workspace regex rules, paired with their map key.
    #[serde(skip)]
    workspace_patterns: Vec<(regex::Regex, String)>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    pub switch_immediate: bool,
    pub return_debounce_secs: u64,
    pub untracked_grace_secs: u64,
    pub entity_tag_key: String,
    pub extra_tags: Vec<String>,
    pub placeholder_activity: String,
    pub placeholder_description: String,
    /// How many days back to look for a prior fact of the same entity to clone
    /// on resume before falling back to a placeholder. `0` = today only.
    pub resume_lookback_days: u64,
    /// When a resume clones a prior fact's description, prepend the continued
    /// marker (`..`) to each carried-over block's first line — but only when the
    /// cloned fact is from the same calendar day. A new day always starts fresh.
    pub mark_continuations: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            switch_immediate: true,
            return_debounce_secs: 60,
            untracked_grace_secs: 60,
            entity_tag_key: "entity".into(),
            extra_tags: Vec::new(),
            placeholder_activity: "placeholder".into(),
            placeholder_description: "auto-started by jiji-hamster-bridge — rename me".into(),
            resume_lookback_days: 5,
            mark_continuations: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityRule {
    /// Entity tag value; defaults to the activity name.
    pub entity: Option<String>,
    /// Hamster category used when creating placeholder facts for this entity.
    pub category: String,
    pub placeholder_activity: Option<String>,
    pub placeholder_description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRule {
    /// Track this workspace under the given entity (overrides the activity).
    pub entity: Option<String>,
    /// `false` = explicitly untracked even inside a tracked activity.
    pub track: Option<bool>,
    /// Placeholder category; falls back to the activity rule with the same entity.
    pub category: Option<String>,
    pub placeholder_activity: Option<String>,
    pub placeholder_description: Option<String>,
}

impl Config {
    pub fn parse(s: &str) -> anyhow::Result<Config> {
        let mut cfg: Config = toml::from_str(s).context("invalid TOML")?;
        cfg.validate()?;
        cfg.build_patterns()?;
        Ok(cfg)
    }

    fn validate(&self) -> anyhow::Result<()> {
        for (name, w) in &self.workspaces {
            if w.track == Some(true) {
                bail!(
                    "workspace rule '{name}': track = true is implied; omit the field \
                     (track = false is the only meaningful value)"
                );
            }
            match (&w.entity, w.track) {
                (Some(_), Some(false)) => {
                    bail!("workspace rule '{name}': entity and track=false are mutually exclusive")
                }
                (None, Some(false)) => {}
                (Some(e), _) => {
                    let resolvable =
                        w.category.is_some() || self.activity_rule_for_entity(e).is_some();
                    if !resolvable {
                        bail!(
                            "workspace rule '{name}': entity '{e}' has no category \
                             (add category here or an [activities.*] rule with this entity)"
                        );
                    }
                }
                (None, _) => {
                    bail!("workspace rule '{name}': needs either entity or track = false")
                }
            }
        }
        Ok(())
    }

    /// Compile every regex-form key into the pattern caches. Runs after
    /// `validate`, so exact-rule invariants already hold. Errors here propagate
    /// out of `parse` and are surfaced by the hot-reload path as a notification.
    fn build_patterns(&mut self) -> anyhow::Result<()> {
        for (key, rule) in &self.activities {
            let Some(src) = regex_source(key) else {
                continue;
            };
            if src.is_empty() {
                bail!("activity rule '{key}': empty regex matches everything");
            }
            if rule.entity.is_none() {
                bail!("activity rule '{key}': regex rules require an explicit entity");
            }
            let re = regex::Regex::new(src)
                .with_context(|| format!("activity rule '{key}': invalid regex"))?;
            self.activity_patterns.push((re, key.clone()));
        }
        for key in self.workspaces.keys() {
            let Some(src) = regex_source(key) else {
                continue;
            };
            if src.is_empty() {
                bail!("workspace rule '{key}': empty regex matches everything");
            }
            let re = regex::Regex::new(src)
                .with_context(|| format!("workspace rule '{key}': invalid regex"))?;
            self.workspace_patterns.push((re, key.clone()));
        }
        Ok(())
    }

    /// Resolve placeholder parameters for an entity, wherever it is defined:
    /// activity rules first, then workspace rules carrying an inline category.
    pub fn rule_for_entity(&self, entity: &str) -> Option<TrackedRule> {
        if let Some(a) = self.activity_rule_for_entity(entity) {
            return Some(TrackedRule {
                entity: entity.to_string(),
                category: a.category.clone(),
                placeholder_activity: a
                    .placeholder_activity
                    .clone()
                    .unwrap_or_else(|| self.defaults.placeholder_activity.clone()),
                placeholder_description: a
                    .placeholder_description
                    .clone()
                    .unwrap_or_else(|| self.defaults.placeholder_description.clone()),
            });
        }
        self.workspaces.values().find_map(|w| {
            (w.entity.as_deref() == Some(entity) && w.category.is_some()).then(|| TrackedRule {
                entity: entity.to_string(),
                category: w.category.clone().unwrap(),
                placeholder_activity: w
                    .placeholder_activity
                    .clone()
                    .unwrap_or_else(|| self.defaults.placeholder_activity.clone()),
                placeholder_description: w
                    .placeholder_description
                    .clone()
                    .unwrap_or_else(|| self.defaults.placeholder_description.clone()),
            })
        })
    }

    /// The first activity rule whose effective entity matches.
    pub fn activity_rule_for_entity(&self, entity: &str) -> Option<&ActivityRule> {
        self.activities
            .iter()
            .find(|(name, r)| r.entity.as_deref().unwrap_or(name) == entity)
            .map(|(_, r)| r)
    }

    /// Build the tracked rule for a matched workspace rule (`None` = untracked).
    fn tracked_from_workspace(&self, w: &WorkspaceRule) -> Option<TrackedRule> {
        match (&w.entity, w.track) {
            (_, Some(false)) => None,
            (Some(entity), _) => {
                let base = self.activity_rule_for_entity(entity);
                Some(TrackedRule {
                    entity: entity.clone(),
                    category: w
                        .category
                        .clone()
                        .or_else(|| base.map(|r| r.category.clone()))
                        .expect("validated: category resolvable"),
                    placeholder_activity: w
                        .placeholder_activity
                        .clone()
                        .or_else(|| base.and_then(|r| r.placeholder_activity.clone()))
                        .unwrap_or_else(|| self.defaults.placeholder_activity.clone()),
                    placeholder_description: w
                        .placeholder_description
                        .clone()
                        .or_else(|| base.and_then(|r| r.placeholder_description.clone()))
                        .unwrap_or_else(|| self.defaults.placeholder_description.clone()),
                })
            }
            (None, _) => unreachable!("validated: entity xor track=false"),
        }
    }

    /// Build the tracked rule for a matched activity rule. For regex rules the
    /// entity is always explicit (validated); for exact rules it defaults to the
    /// matched activity name, as before.
    fn tracked_from_activity(&self, name: &str, a: &ActivityRule) -> TrackedRule {
        TrackedRule {
            entity: a.entity.clone().unwrap_or_else(|| name.to_string()),
            category: a.category.clone(),
            placeholder_activity: a
                .placeholder_activity
                .clone()
                .unwrap_or_else(|| self.defaults.placeholder_activity.clone()),
            placeholder_description: a
                .placeholder_description
                .clone()
                .unwrap_or_else(|| self.defaults.placeholder_description.clone()),
        }
    }

    /// Resolve the effective tracking rule in four-tier precedence: workspace
    /// exact, then workspace regex, then activity exact, then activity regex.
    /// Exact beats regex within each domain; the first regex in sorted-key order
    /// wins. `None` = untracked.
    pub fn effective(&self, ctx: &crate::events::Context) -> Option<TrackedRule> {
        if let Some(ws_name) = &ctx.workspace {
            if let Some(w) = self.workspaces.get(ws_name) {
                return self.tracked_from_workspace(w);
            }
            if let Some(w) = self
                .workspace_patterns
                .iter()
                .find(|(re, _)| re.is_match(ws_name))
                .map(|(_, k)| &self.workspaces[k])
            {
                return self.tracked_from_workspace(w);
            }
        }
        let name = ctx.activity.as_ref()?;
        let a = self.activities.get(name).or_else(|| {
            self.activity_patterns
                .iter()
                .find(|(re, _)| re.is_match(name))
                .map(|(_, k)| &self.activities[k])
        })?;
        Some(self.tracked_from_activity(name, a))
    }
}

/// A fully resolved "track this" decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedRule {
    pub entity: String,
    pub category: String,
    pub placeholder_activity: String,
    pub placeholder_description: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Context;

    const FULL: &str = r#"
        [defaults]
        switch_immediate = true
        return_debounce_secs = 60
        untracked_grace_secs = 60
        entity_tag_key = "entity"
        extra_tags = ["location: home"]
        placeholder_activity = "placeholder"
        placeholder_description = "auto-started by jiji-hamster-bridge — rename me"

        [activities.work1]
        entity = "work1"
        category = "work1.example"

        [activities.work2]
        category = "work2.example"
        placeholder_activity = "support"

        [workspaces.invoicing]
        entity = "work1"

        [workspaces.scratch]
        track = false
    "#;

    #[test]
    fn regex_source_detects_slash_delimited_keys() {
        assert_eq!(regex_source("/acme/"), Some("acme"));
        assert_eq!(regex_source("/^acme-/"), Some("^acme-"));
        assert_eq!(regex_source("//"), Some("")); // empty source — caught later
        assert_eq!(regex_source("acme"), None); // ordinary literal
        assert_eq!(regex_source("/"), None); // too short
        assert_eq!(regex_source("/acme"), None); // not closed
        assert_eq!(regex_source(""), None);
    }

    #[test]
    fn valid_regex_activity_config_parses() {
        let s = "[activities.\"/acme/\"]\nentity = \"acme_other\"\ncategory = \"acme-corp.com\"\n";
        assert!(Config::parse(s).is_ok());
    }

    #[test]
    fn regex_activity_requires_entity() {
        // no entity → the matched name has no sensible default, reject at parse
        let s = "[activities.\"/acme/\"]\ncategory = \"acme-corp.com\"\n";
        assert!(Config::parse(s).is_err());
    }

    #[test]
    fn empty_regex_rejected() {
        let s = "[activities.\"//\"]\nentity = \"x\"\ncategory = \"c\"\n";
        assert!(Config::parse(s).is_err());
    }

    #[test]
    fn invalid_regex_rejected() {
        // unbalanced paren — does not compile
        let s = "[activities.\"/(/\"]\nentity = \"x\"\ncategory = \"c\"\n";
        assert!(Config::parse(s).is_err());
    }

    #[test]
    fn parses_full_config() {
        let c = Config::parse(FULL).unwrap();
        assert_eq!(c.defaults.return_debounce_secs, 60);
        assert_eq!(c.activities["work1"].category, "work1.example");
        // entity field absent → effective entity is the activity name "work2"
        assert_eq!(c.activities["work2"].entity.as_deref(), None);
        assert_eq!(c.workspaces["invoicing"].entity.as_deref(), Some("work1"));
        assert_eq!(c.workspaces["scratch"].track, Some(false));
    }

    #[test]
    fn minimal_config_gets_defaults() {
        let c = Config::parse("[activities.work1]\ncategory = \"y\"\n").unwrap();
        assert!(c.defaults.switch_immediate);
        assert_eq!(c.defaults.return_debounce_secs, 60);
        assert_eq!(c.defaults.untracked_grace_secs, 60);
        assert_eq!(c.defaults.entity_tag_key, "entity");
        assert!(c.defaults.extra_tags.is_empty());
        assert_eq!(c.defaults.placeholder_activity, "placeholder");
        assert_eq!(
            c.defaults.placeholder_description,
            "auto-started by jiji-hamster-bridge — rename me"
        );
        assert_eq!(c.defaults.resume_lookback_days, 5);
        assert!(c.defaults.mark_continuations);
    }

    #[test]
    fn mark_continuations_can_be_disabled() {
        let c = Config::parse(
            "[defaults]\nmark_continuations = false\n[activities.work1]\ncategory = \"y\"\n",
        )
        .unwrap();
        assert!(!c.defaults.mark_continuations);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::parse("[defaults]\nbogus = 1\n").is_err());
    }

    #[test]
    fn rejects_workspace_rule_with_both_entity_and_track_false() {
        let s = "[workspaces.x]\nentity = \"y\"\ntrack = false\n";
        assert!(Config::parse(s).is_err());
    }

    #[test]
    fn rejects_workspace_rule_with_neither_entity_nor_track_false() {
        assert!(Config::parse("[workspaces.x]\n").is_err());
    }

    #[test]
    fn rejects_workspace_entity_with_unresolvable_category() {
        // entity "ghost" has no [activities.*] rule providing a category
        let s = "[workspaces.x]\nentity = \"ghost\"\n";
        assert!(Config::parse(s).is_err());
        // but an inline category fixes it
        let s2 = "[workspaces.x]\nentity = \"ghost\"\ncategory = \"g.com\"\n";
        assert!(Config::parse(s2).is_ok());
    }

    #[test]
    fn rejects_workspace_track_true() {
        assert!(Config::parse("[workspaces.x]\ntrack = true\n").is_err());
        assert!(
            Config::parse("[workspaces.x]\nentity = \"y\"\ncategory = \"c\"\ntrack = true\n")
                .is_err()
        );
    }

    fn ctx(activity: Option<&str>, workspace: Option<&str>) -> Context {
        Context {
            activity: activity.map(Into::into),
            workspace: workspace.map(Into::into),
        }
    }

    #[test]
    fn activity_rule_applies_with_entity_defaulting_to_name() {
        let c = Config::parse(FULL).unwrap();
        let r = c.effective(&ctx(Some("work2"), None)).unwrap();
        assert_eq!(r.entity, "work2");
        assert_eq!(r.category, "work2.example");
        assert_eq!(r.placeholder_activity, "support"); // per-rule override
    }

    #[test]
    fn unconfigured_activity_is_untracked() {
        let c = Config::parse(FULL).unwrap();
        assert!(c.effective(&ctx(Some("games"), None)).is_none());
        assert!(c.effective(&ctx(None, None)).is_none());
    }

    #[test]
    fn workspace_rule_overrides_activity_even_untracked_one() {
        let c = Config::parse(FULL).unwrap();
        // tracked workspace inside an untracked activity
        let r = c.effective(&ctx(Some("games"), Some("invoicing"))).unwrap();
        assert_eq!(r.entity, "work1");
        assert_eq!(r.category, "work1.example"); // borrowed from activities.work1
    }

    #[test]
    fn track_false_workspace_wins_inside_tracked_activity() {
        let c = Config::parse(FULL).unwrap();
        assert!(c.effective(&ctx(Some("work1"), Some("scratch"))).is_none());
    }

    #[test]
    fn unnamed_or_unconfigured_workspace_falls_through_to_activity() {
        let c = Config::parse(FULL).unwrap();
        let r = c.effective(&ctx(Some("work1"), Some("random-ws"))).unwrap();
        assert_eq!(r.entity, "work1");
        let r2 = c.effective(&ctx(Some("work1"), None)).unwrap();
        assert_eq!(r2.entity, "work1");
    }

    #[test]
    fn workspace_rule_with_inline_category_needs_no_activity_rule() {
        let c = Config::parse("[workspaces.lab]\nentity = \"ghost\"\ncategory = \"ghost.org\"\n")
            .unwrap();
        let r = c.effective(&ctx(None, Some("lab"))).unwrap();
        assert_eq!(r.entity, "ghost");
        assert_eq!(r.category, "ghost.org");
        assert_eq!(r.placeholder_activity, "placeholder"); // from defaults
    }

    #[test]
    fn regex_activity_matches_non_exact_name() {
        let s = "[activities.\"/acme/\"]\nentity = \"acme_other\"\ncategory = \"acme-corp.com\"\n";
        let c = Config::parse(s).unwrap();
        let r = c.effective(&ctx(Some("acme-invoices"), None)).unwrap();
        assert_eq!(r.entity, "acme_other");
        assert_eq!(r.category, "acme-corp.com");
    }

    #[test]
    fn exact_activity_beats_regex() {
        let s = "[activities.acme]\ncategory = \"acme-corp.com\"\n\
                 [activities.\"/acme/\"]\nentity = \"acme_other\"\ncategory = \"acme-corp.com\"\n";
        let c = Config::parse(s).unwrap();
        // exact name hits the literal rule, not the (also-matching) regex
        let r = c.effective(&ctx(Some("acme"), None)).unwrap();
        assert_eq!(r.entity, "acme");
    }

    #[test]
    fn first_regex_in_sorted_order_wins() {
        // both "/^a/" and "/acme/" match "acme-x"; sorted-key order: "/^a/" < "/acme/"
        let s = "[activities.\"/^a/\"]\nentity = \"first\"\ncategory = \"c\"\n\
                 [activities.\"/acme/\"]\nentity = \"second\"\ncategory = \"c\"\n";
        let c = Config::parse(s).unwrap();
        let r = c.effective(&ctx(Some("acme-x"), None)).unwrap();
        assert_eq!(r.entity, "first");
    }

    #[test]
    fn regex_workspace_track_false_untracks() {
        let s = "[activities.acme]\ncategory = \"acme-corp.com\"\n\
                 [workspaces.\"/^scratch-/\"]\ntrack = false\n";
        let c = Config::parse(s).unwrap();
        assert!(c.effective(&ctx(Some("acme"), Some("scratch-1"))).is_none());
    }

    #[test]
    fn regex_workspace_overrides_activity() {
        let s = "[activities.work2]\ncategory = \"general\"\n\
                 [workspaces.\"/^bill/\"]\nentity = \"acme\"\ncategory = \"acme-corp.com\"\n";
        let c = Config::parse(s).unwrap();
        // untracked activity, but the workspace regex pins it to acme
        let r = c.effective(&ctx(Some("games"), Some("billing"))).unwrap();
        assert_eq!(r.entity, "acme");
        assert_eq!(r.category, "acme-corp.com");
    }

    #[test]
    fn rule_for_entity_resolves_regex_rule_entity() {
        let s = "[activities.\"/acme/\"]\nentity = \"acme_other\"\ncategory = \"acme-corp.com\"\n";
        let c = Config::parse(s).unwrap();
        let r = c.rule_for_entity("acme_other").unwrap();
        assert_eq!(r.category, "acme-corp.com");
    }

    #[test]
    fn rule_for_entity_resolves_workspace_only_entities() {
        let c =
            Config::parse("[workspaces.x]\nentity = \"ghost\"\ncategory = \"g.com\"\n").unwrap();
        assert_eq!(c.rule_for_entity("ghost").unwrap().category, "g.com");
        assert!(c.rule_for_entity("nope").is_none());
    }

    #[test]
    fn exact_workspace_beats_regex_workspace() {
        // both [workspaces.billing] and the regex "/^bill/" match "billing";
        // the exact rule (entity "exact_e") must win over the regex (entity "regex_e")
        let s = "[workspaces.billing]\nentity = \"exact_e\"\ncategory = \"exact.com\"\n\
                 [workspaces.\"/^bill/\"]\nentity = \"regex_e\"\ncategory = \"regex.com\"\n";
        let c = Config::parse(s).unwrap();
        let r = c.effective(&ctx(None, Some("billing"))).unwrap();
        assert_eq!(r.entity, "exact_e");
        assert_eq!(r.category, "exact.com");
    }

    #[test]
    fn regex_workspace_derives_category_from_activity_rule() {
        // regex workspace has entity but no inline category; the category is
        // borrowed from the [activities.acme] rule sharing that entity
        let s = "[activities.acme]\ncategory = \"acme-corp.com\"\n\
                 [workspaces.\"/^bill/\"]\nentity = \"acme\"\n";
        let c = Config::parse(s).unwrap();
        let r = c.effective(&ctx(None, Some("billing"))).unwrap();
        assert_eq!(r.entity, "acme");
        assert_eq!(r.category, "acme-corp.com");
    }

    #[test]
    fn non_matching_regex_workspace_falls_through_to_activity() {
        // "/^foo/" does not match "other"; resolution must fall through to the
        // tracked activity "bar" rather than returning None
        let s = "[activities.bar]\nentity = \"bar\"\ncategory = \"bar.com\"\n\
                 [workspaces.\"/^foo/\"]\nentity = \"bar\"\n";
        let c = Config::parse(s).unwrap();
        let r = c.effective(&ctx(Some("bar"), Some("other"))).unwrap();
        assert_eq!(r.entity, "bar");
        assert_eq!(r.category, "bar.com");
    }
}
