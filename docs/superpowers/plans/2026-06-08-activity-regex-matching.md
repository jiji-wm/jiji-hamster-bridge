# Regex activity / workspace matching — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a mapping key in `config.toml` be a slash-delimited regex (`[activities."/acme/"]`) that matches activity/workspace names as a fallback after exact-key lookup.

**Architecture:** All changes are inside `src/config.rs`. A key wrapped in `/…/` is a regex; regexes are compiled once at parse time into two `#[serde(skip)]` caches (`activity_patterns`, `workspace_patterns`) and consulted by `Config::effective` only after exact lookups miss. Exact always beats regex. A bad/empty regex (or an entity-less activity regex) makes `Config::parse` return `Err`, which the existing hot-reload path already turns into a notification while keeping the previous config.

**Tech Stack:** Rust (edition 2024), `regex` crate, `toml`/`serde`, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-06-08-activity-regex-matching-design.md`

**Working directory for all commands:** the crate root `~/projects/desktop/de/jiji/repos/jiji-hamster-bridge` (branch `feat/activity-regex-matching`).

---

## File structure

- **Modify** `Cargo.toml` — add `regex = "1"` dependency.
- **Modify** `src/config.rs` — the regex key helper, the two pattern caches, `build_patterns`, parse wiring, validation, the `effective` refactor + regex sweeps, and all new unit tests (they live in the existing `#[cfg(test)] mod tests`).
- **Modify** `README.md` — document the `/…/` regex key form.
- **Modify (separate repo: chezmoi)** `~/.local/share/chezmoi/dot_config/jiji-hamster-bridge/config.toml` — add a commented example.

---

## Task 1: Add the `regex` dependency

**Files:**
- Modify: `Cargo.toml:14-25` (the `[dependencies]` table)

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, add this line to `[dependencies]` (keep the table alphabetically ordered — insert between `notify = "8"` and `serde = …`):

```toml
regex = "1"
```

- [ ] **Step 2: Verify it resolves and builds**

Run: `cargo build`
Expected: compiles successfully; `regex` and its transitive deps appear in `Cargo.lock`.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add regex dependency for pattern matching

Review-Needed: committed by Claude Code
AI-Assisted: one-shot (claude-opus-4-8)"
```

---

## Task 2: Regex-form key detection helper

A key is regex-form iff it starts and ends with `/` and has length ≥ 2. The helper returns the inner source (possibly empty, so the build step can reject `//` explicitly).

**Files:**
- Modify: `src/config.rs` (add a free function near the top of the file, after the `use` lines around line 6)
- Test: `src/config.rs` (the `#[cfg(test)] mod tests` block at the bottom)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/config.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test regex_source_detects_slash_delimited_keys`
Expected: FAIL — `cannot find function 'regex_source' in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add this free function to `src/config.rs`, just below the `use serde::Deserialize;` line:

```rust
/// If `key` is a regex-form mapping key `/source/`, return the inner `source`
/// (which may be empty). Ordinary literal keys return `None`. `/` is ASCII, so
/// byte checks and the slice are always on char boundaries.
fn regex_source(key: &str) -> Option<&str> {
    let b = key.as_bytes();
    (b.len() >= 2 && b[0] == b'/' && b[b.len() - 1] == b'/').then(|| &key[1..key.len() - 1])
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test regex_source_detects_slash_delimited_keys`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: detect slash-delimited regex keys in config

Review-Needed: committed by Claude Code
AI-Assisted: one-shot (claude-opus-4-8)"
```

---

## Task 3: Compile + validate regex rules at parse time

Add the two pattern caches, a `build_patterns` step that compiles every regex-form key, and wire it into `Config::parse`. This task also enforces the two regex-specific validation rules (non-empty source; activity regex rules require an explicit entity).

**Files:**
- Modify: `src/config.rs:13-20` (the `Config` struct — add two fields)
- Modify: `src/config.rs:73-77` (`Config::parse` — call `build_patterns`)
- Modify: `src/config.rs` (add the `build_patterns` method inside `impl Config`)
- Test: `src/config.rs` `mod tests`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test regex_activity_requires_entity empty_regex_rejected invalid_regex_rejected valid_regex_activity_config_parses`
Expected: FAIL — `valid_regex_activity_config_parses` passes by accident (no compilation yet), but the three negative tests FAIL because nothing rejects them. (If the struct fields below aren't added first the file won't compile — add Step 3's field + method, then re-run.)

- [ ] **Step 3: Add the cache fields to `Config`**

In `src/config.rs`, the `Config` struct (currently lines 13-20) becomes:

```rust
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
```

- [ ] **Step 4: Add `build_patterns` and wire it into `parse`**

Change `Config::parse` (currently lines 73-77) to:

```rust
pub fn parse(s: &str) -> anyhow::Result<Config> {
    let mut cfg: Config = toml::from_str(s).context("invalid TOML")?;
    cfg.validate()?;
    cfg.build_patterns()?;
    Ok(cfg)
}
```

Add this method inside `impl Config` (place it right after `validate`):

```rust
/// Compile every regex-form key into the pattern caches. Runs after
/// `validate`, so exact-rule invariants already hold. Errors here propagate
/// out of `parse` and are surfaced by the hot-reload path as a notification.
fn build_patterns(&mut self) -> anyhow::Result<()> {
    for (key, rule) in &self.activities {
        let Some(src) = regex_source(key) else { continue };
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
    for (key, _) in &self.workspaces {
        let Some(src) = regex_source(key) else { continue };
        if src.is_empty() {
            bail!("workspace rule '{key}': empty regex matches everything");
        }
        let re = regex::Regex::new(src)
            .with_context(|| format!("workspace rule '{key}': invalid regex"))?;
        self.workspace_patterns.push((re, key.clone()));
    }
    Ok(())
}
```

(Workspace regex rules need no entity check here: `validate` already requires every workspace rule to have either `entity` or `track = false`, and it runs first.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test`
Expected: the four new tests PASS and all pre-existing tests still PASS.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat: compile and validate regex config rules at parse time

Review-Needed: committed by Claude Code
AI-Assisted: one-shot (claude-opus-4-8)"
```

---

## Task 4: Extract `TrackedRule` construction helpers (refactor, no behavior change)

`effective` builds a `TrackedRule` inline for both the workspace and activity paths. Extract two helpers so the upcoming regex paths can reuse the exact same construction. Existing tests are the safety net.

**Files:**
- Modify: `src/config.rs` `effective` (currently lines 152-196) and add two private helpers in `impl Config`.

- [ ] **Step 1: Add the two helpers**

Add to `impl Config`:

```rust
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
```

- [ ] **Step 2: Rewrite `effective` to use the helpers (still exact-only)**

Replace the body of `effective` (lines 152-196) with:

```rust
pub fn effective(&self, ctx: &crate::events::Context) -> Option<TrackedRule> {
    if let Some(ws_name) = &ctx.workspace
        && let Some(w) = self.workspaces.get(ws_name)
    {
        return self.tracked_from_workspace(w);
    }
    let name = ctx.activity.as_ref()?;
    let a = self.activities.get(name)?;
    Some(self.tracked_from_activity(name, a))
}
```

- [ ] **Step 3: Run the full suite to verify no behavior change**

Run: `cargo test`
Expected: ALL existing tests PASS (this is a pure refactor).

- [ ] **Step 4: Commit**

```bash
git add src/config.rs
git commit -m "refactor: extract TrackedRule construction helpers

Review-Needed: committed by Claude Code
AI-Assisted: one-shot (claude-opus-4-8)"
```

---

## Task 5: Add the regex sweeps to `effective`

Now insert the two regex fallbacks: workspace-regex (after workspace-exact) and activity-regex (after activity-exact). Exact still wins because it is checked first.

**Files:**
- Modify: `src/config.rs` `effective`
- Test: `src/config.rs` `mod tests`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests`:

```rust
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
    // both "/^y/" and "/acme/" match "acme-x"; sorted-key order: "/^y/" < "/acme/"
    let s = "[activities.\"/^y/\"]\nentity = \"first\"\ncategory = \"c\"\n\
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
    assert!(
        c.effective(&ctx(Some("acme"), Some("scratch-1")))
            .is_none()
    );
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test regex_activity_matches_non_exact_name exact_activity_beats_regex first_regex_in_sorted_order_wins regex_workspace_track_false_untracks regex_workspace_overrides_activity`
Expected: FAIL — the regex names fall through to untracked (`effective` still exact-only), so the `unwrap()`s panic / `is_none` assertions invert.

- [ ] **Step 3: Add the regex sweeps to `effective`**

Replace `effective` with:

```rust
pub fn effective(&self, ctx: &crate::events::Context) -> Option<TrackedRule> {
    if let Some(ws_name) = &ctx.workspace {
        if let Some(w) = self.workspaces.get(ws_name) {
            return self.tracked_from_workspace(w);
        }
        if let Some(key) = self
            .workspace_patterns
            .iter()
            .find(|(re, _)| re.is_match(ws_name))
            .map(|(_, k)| k)
        {
            return self.tracked_from_workspace(&self.workspaces[key]);
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
```

(Note: when an exact OR regex workspace rule matches, the function returns from inside the `if let Some(ws_name)` block — including `None` for `track=false`. If *no* workspace rule matches, it falls through to the activity lookup, exactly as before.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: the five new tests PASS and every pre-existing test still PASSES.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: match activity/workspace names by regex as exact-key fallback

Review-Needed: committed by Claude Code
AI-Assisted: one-shot (claude-opus-4-8)"
```

---

## Task 6: Guard the execute-time entity lookup for regex rules

At fact-creation time, `run.rs` calls `rule_for_entity(entity)` with the resolved entity (e.g. `acme_other`) to find its category. Because regex rules carry an explicit `entity`, the existing `activity_rule_for_entity` reverse lookup already resolves them — this test locks that in so a future change can't silently break the placeholder path.

**Files:**
- Test: `src/config.rs` `mod tests`

- [ ] **Step 1: Write the guard test**

Add to `mod tests`:

```rust
#[test]
fn rule_for_entity_resolves_regex_rule_entity() {
    let s = "[activities.\"/acme/\"]\nentity = \"acme_other\"\ncategory = \"acme-corp.com\"\n";
    let c = Config::parse(s).unwrap();
    let r = c.rule_for_entity("acme_other").unwrap();
    assert_eq!(r.category, "acme-corp.com");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test rule_for_entity_resolves_regex_rule_entity`
Expected: PASS immediately — this is a characterization/guard test confirming the reverse lookup already works for regex rules (their entity is explicit). If it FAILS, the regex rule's entity is not being indexed and `effective`/`build_patterns` from Tasks 3/5 need revisiting.

- [ ] **Step 3: Commit**

```bash
git add src/config.rs
git commit -m "test: guard execute-time entity lookup for regex rules

Review-Needed: committed by Claude Code
AI-Assisted: one-shot (claude-opus-4-8)"
```

---

## Task 7: Full check + clippy

**Files:** none (verification only)

- [ ] **Step 1: Run the whole suite**

Run: `cargo test`
Expected: all tests PASS.

- [ ] **Step 2: Lint**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings. (If clippy flags the unused-binding in earlier helpers, fix inline and re-run.)

- [ ] **Step 3: Commit only if clippy required a fix**

```bash
git add -A
git commit -m "chore: satisfy clippy for regex matching

Review-Needed: committed by Claude Code
AI-Assisted: one-shot (claude-opus-4-8)"
```

(If nothing changed, skip the commit.)

---

## Task 8: Documentation

Document the new `/…/` key form in the README and in the deployed chezmoi config example. The chezmoi file is a **separate git repo** — commit it there.

**Files:**
- Modify: `README.md` (the `## Configuration` TOML block, ~lines 9-37)
- Modify (separate repo): `~/.local/share/chezmoi/dot_config/jiji-hamster-bridge/config.toml`

- [ ] **Step 1: Update the README**

In `README.md`, immediately after the `[activities.work2]` block inside the Configuration example, add:

```toml
[activities."/acme/"]           # regex key (slash-delimited) — matches any
entity = "acme_other"           # activity name containing "acme" that no
category = "acme-corp.com"      # exact rule already matched. entity REQUIRED.
```

Then add this paragraph after the example block:

```markdown
A mapping key wrapped in `/…/` is a regex (Rust `regex` syntax, matched
unanchored with `is_match`). Exact keys are always tried first; regex keys are a
fallback, evaluated in sorted-key order with the first match winning. Use `^`/`$`
to anchor (`/^acme-/`). Regex **activity** rules must set `entity` explicitly
(there is no single name to default to); regex **workspace** rules follow the
same rules as exact ones (`entity`, or `track = false`). An invalid or empty
regex is rejected on load like any other config error.
```

- [ ] **Step 2: Commit the README (crate repo)**

```bash
git add README.md
git commit -m "docs: document regex mapping keys

Review-Needed: committed by Claude Code
AI-Assisted: one-shot (claude-opus-4-8)"
```

- [ ] **Step 3: Update the chezmoi config example**

In `~/.local/share/chezmoi/dot_config/jiji-hamster-bridge/config.toml`, after the `[activities.work2]` block, add a commented example:

```toml
# Regex keys (slash-delimited) match as a fallback after exact keys. entity is
# required on activity regex rules. Example: track any other acme-* activity
# under a separate entity:
#
# [activities."/^acme-/"]
# entity = "acme_other"
# category = "acme-corp.com"
```

- [ ] **Step 4: Commit in the chezmoi repo**

```bash
cd ~/.local/share/chezmoi
git add dot_config/jiji-hamster-bridge/config.toml
git commit -m "jiji-hamster-bridge: document regex activity keys

Review-Needed: committed by Claude Code
AI-Assisted: one-shot (claude-opus-4-8)"
```

(Do **not** run `chezmoi apply` or push — the user does that.)

---

## Done criteria

- `cargo test` green, `cargo clippy --all-targets -- -D warnings` clean.
- Exact-match behavior unchanged (all pre-existing tests pass untouched).
- `[activities."/acme/"]` with an explicit entity tracks non-exact matches; exact keys win; bad/empty/entity-less regex rules are rejected on load.
- README + chezmoi example document the feature.
