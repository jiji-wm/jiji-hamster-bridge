# Regex activity / workspace matching — design

**Date:** 2026-06-08
**Status:** approved (design), pending implementation
**Component:** `src/config.rs` (matching), `Cargo.toml` (new `regex` dep)

## Problem

Today the bridge maps a jiji **activity name** (or named **workspace**) to a
hamster **entity** by *exact string equality*. The decisive lookups are:

- `config.rs:183` — `self.activities.get(name)` (activity → rule)
- `config.rs:154` — `self.workspaces.get(ws_name)` (workspace → rule)

So an activity literally named `acme` matches `[activities.acme]`, but
`acme-invoices`, `acme-rd`, `acme2`, etc. fall through to *untracked*. The
user wants to keep the existing one-entry-per-mapping model but allow a
mapping's **key to be a regex**, so that e.g. any activity name containing
`acme` (other than the exact `acme`) can be tracked under a chosen entity.

Concretely, the user wants:

```
activity "acme"   → entity acme          (exact, wins)
activity /acme/    → entity acme_other    (regex fallback)
```

## Goals

- Add regex matching to the **existing** activity and workspace mapping tables.
- **Exact keys always take precedence** over regex keys.
- Each mapping entry still names its own target entity — no new "collapse vs.
  keep-name" behaviour; a regex entry simply states its `entity` explicitly.
- Malformed regexes are rejected at config-parse time and surface through the
  existing hot-reload error path (notification + previous config stays active).

## Non-goals

- Glob syntax (`acme*`). Regex subsumes it (`/^acme/`); supporting two
  pattern languages adds ambiguity (`*` means different things) for no gain.
- Capturing groups / templating the entity from the matched name. Entities are
  fixed strings on the rule. (Could be a future extension; YAGNI now.)
- Changing the workspace-vs-activity precedence model or the engine/resume
  machinery. This is purely a matching change inside `Config`.

## Syntax (Approach A — slash-delimited key)

A mapping key wrapped in `/…/` is interpreted as a regex; any other key is an
exact literal, exactly as today.

```toml
[activities.acme]            # exact — checked first, wins
category = "acme-corp.com"

[activities."/acme/"]        # regex fallback — note: entity is REQUIRED
entity = "acme_other"
category = "acme-corp.com"

[workspaces."/^scratch-/"]    # regex workspaces work identically
track = false
```

- The inner text (`acme`, `^scratch-`) is a standard Rust `regex::Regex`
  source, matched with `Regex::is_match` against the activity/workspace name.
  Matching is therefore **unanchored / substring** unless the pattern anchors
  itself. User examples map cleanly:
  - `acme*`   → write `/^acme/`
  - `/^acme-/` → start-anchored
  - `/acme/`  → substring anywhere
- A key is "regex-form" iff it starts with `/` and ends with `/` and has length
  ≥ 2. The empty-source key `//` is regex-form but is **rejected explicitly**
  in the build step (an empty `regex::Regex` compiles fine and would match every
  name — a footgun, not a feature). Real jiji activity/workspace names never
  start and end with `/`, so there is no collision with literal names.

### Precedence

Evaluated in `Config::effective`, highest first:

1. **workspace exact** — `workspaces.get(ws_name)`
2. **workspace regex** — first regex-form workspace key whose pattern matches
   `ws_name`, in sorted-key order
3. **activity exact** — `activities.get(name)`
4. **activity regex** — first regex-form activity key whose pattern matches
   `name`, in sorted-key order

Exact always beats regex because exact lookups (1, 3) run before the regex
sweeps (2, 4). Among multiple matching regexes within a tier, **sorted key
order, first match wins** — documented; users should keep patterns
non-overlapping. (Rationale: deterministic and dependency-free; no attempt to
rank by specificity, which has no well-defined total order.)

## Data model & compilation

`Config` is `Deserialize` + `Clone` and is rebuilt from scratch on every
hot-reload, so regexes are compiled once at parse time and cached.

- The raw `activities` / `workspaces` `BTreeMap`s keep **all** keys (exact and
  regex-form) — this is what lets the existing entity→category reverse lookups
  (`activity_rule_for_entity`, `rule_for_entity`) continue to work unchanged for
  regex rules (they key on the rule's explicit `entity`, not the map key).
- Add two non-serialized, parse-time-populated caches holding the compiled
  patterns paired with their map key:

  ```rust
  #[serde(skip)]
  activity_patterns: Vec<(regex::Regex, String)>, // (compiled, map key) sorted by key
  #[serde(skip)]
  workspace_patterns: Vec<(regex::Regex, String)>,
  ```

  `regex::Regex` is `Clone` (cheap — internally ref-counted), so `Config: Clone`
  still holds. `#[serde(skip)]` fields default to empty on deserialize and are
  filled by a post-parse build step.

- `Config::parse` flow becomes: `toml::from_str` → `validate()` →
  `build_patterns()` (compile every regex-form key into the caches) → return.
  A compile error in `build_patterns` (or empty source) makes `parse` return
  `Err`, which the file watcher / SIGHUP handler already converts into a
  notification while keeping the previous config — no new error plumbing.

## Validation rules (added to `Config::validate` / build step)

For every **regex-form** key in `activities` and `workspaces`:

- The inner pattern must be non-empty (`//` is rejected:
  `bail!("rule '//': empty regex matches everything")`) and must compile as a
  `regex::Regex` (else `bail!("activity rule '/…/': invalid regex: <err>")`).
- An **activity** regex rule MUST set `entity` explicitly. Without it, the
  effective entity would default to the map key (`/acme/`) — meaningless, and
  it breaks the execute-time `rule_for_entity(entity)` category lookup. So
  `bail!("activity rule '/…/': regex rules require an explicit entity")`.
- A **workspace** regex rule follows the same constraints as an exact one
  (entity xor `track=false`, category resolvable). `track = false` regex
  workspaces are allowed (e.g. `[workspaces."/^scratch-/"]` `track = false`)
  and need no entity, mirroring exact `track=false` rules.

## Matching logic (`Config::effective`)

Insert the two regex sweeps into the existing function:

```rust
pub fn effective(&self, ctx: &crate::events::Context) -> Option<TrackedRule> {
    // 1 + 2: workspace exact, then workspace regex
    if let Some(ws) = &ctx.workspace {
        if let Some(w) = self.workspaces.get(ws) {
            return self.tracked_from_workspace(ws, w);
        }
        if let Some(key) = self.workspace_patterns.iter()
            .find(|(re, _)| re.is_match(ws)).map(|(_, k)| k)
        {
            return self.tracked_from_workspace(ws, &self.workspaces[key]);
        }
    }
    // 3 + 4: activity exact, then activity regex
    let name = ctx.activity.as_ref()?;
    let rule = self.activities.get(name).or_else(|| {
        self.activity_patterns.iter()
            .find(|(re, _)| re.is_match(name))
            .map(|(_, k)| &self.activities[k])
    })?;
    Some(self.tracked_from_activity(name, rule))
}
```

The current inline workspace/activity `TrackedRule` construction (config.rs
156–195) is factored into small helpers (`tracked_from_workspace`,
`tracked_from_activity`) so the exact and regex paths share one body. This also
shrinks `effective`, which is currently doing too much inline.

Note: for an activity regex rule the entity is always explicit (validated), so
`tracked_from_activity` uses `rule.entity` directly rather than defaulting to
`name` — and since the matched `name` is *not* the entity, the downstream
`rule_for_entity(entity)` lookup (used at fact-creation time) resolves against
the rule's explicit entity, which is present in `activities`. No change needed
in `run.rs` / `resume.rs`.

## Testing

Unit tests in `config.rs` (the existing `tests` module already covers exact
matching — extend it):

1. `regex_activity_matches_non_exact_name` — `/acme/` rule with
   `entity = "acme_other"`; `effective(activity="acme-invoices")` →
   entity `acme_other`.
2. `exact_activity_beats_regex` — config with both `[activities.acme]` and
   `[activities."/acme/"]`; `effective(activity="acme")` → entity `acme`
   (the exact rule), not `acme_other`.
3. `regex_activity_requires_entity` — `[activities."/acme/"]` without `entity`
   → `Config::parse` errors.
4. `invalid_regex_rejected` — `[activities."/(/"]` → `parse` errors.
5. `first_regex_in_sorted_order_wins` — two overlapping regex keys; assert the
   sorted-first one is chosen (documents the precedence).
6. `regex_workspace_track_false` — `[workspaces."/^scratch-/"]` `track = false`;
   `effective(activity="acme", workspace="scratch-1")` → `None`.
7. `regex_workspace_overrides_activity` — `[workspaces."/^bill/"]`
   `entity = "acme"`; tracked even inside an untracked activity.
8. `rule_for_entity_resolves_regex_rule_entity` — `rule_for_entity("acme_other")`
   returns the category from the `/acme/` rule (guards the execute-time path).

Existing tests must continue to pass unchanged (no exact-match regression).

## Rollout

- `Cargo.toml`: add `regex = "1"` to `[dependencies]`.
- No config migration needed — existing exact configs are untouched.
- Update `README.md` Configuration section and the deployed
  `dot_config/jiji-hamster-bridge/config.toml` (in the chezmoi dotfiles repo) to
  document the `/…/` regex key form. The chezmoi `config.toml` is a separate
  repo commit.
