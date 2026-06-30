# CLAUDE.md

Repo discipline for Claude Code sessions in this repository.

## What this is

`jiji-hamster-bridge` — a daemon that bridges jiji compositor activity/workspace focus events to GNOME Hamster time tracking over D-Bus. Tracks what you're working on automatically as you switch desktop contexts.

**Dependency contract:** NO `niri-ipc` or `jiji-ipc` Cargo dependency. All compositor interaction is via the `jiji msg --json event-stream` subprocess (`JIJI_MSG_BIN` env override). All Hamster interaction is via the `org.gnome.Hamster` D-Bus service (zbus).

## Build / test / lint

```sh
cargo test                          # 97 unit/integration tests
cargo test -- --ignored             # 1 live smoke test (needs session bus + hamster-service, read-only)
cargo +nightly fmt --all
cargo clippy --all-targets
```

The ignored smoke test connects to a live D-Bus session and a running Hamster service — read-only, safe to run on a live machine but not in CI.

## Architecture

Two layers — **pure core** (no I/O) and **async shell** (all I/O):

**Pure core** (no I/O, freely unit-testable):
- `config.rs` — config types, parsing, validation, precedence rules (workspace overrides activity overrides defaults).
- `events.rs` — compositor event types; `Tracker` maps focus events to context transitions.
- `engine.rs` — hysteresis state machine: starts are immediate, destructive transitions (stop, cross-entity switch) are debounced. Engine time is `tokio::time::Instant` for deterministic paused-time tests — never `std::time::Instant`.
- `resume.rs` — resume planner: given today's facts and the target entity, selects the best matching fact or produces a placeholder spec.

**Async shell** (all I/O, minimal logic):
- `zbus_client.rs` — typed wrappers around the `org.gnome.Hamster` D-Bus interface.
- `run.rs` — main event loop: reads the jiji event stream subprocess, drives the engine, calls zbus_client.
- `main.rs` — CLI parsing, config loading, top-level wiring.

**The rule:** no I/O in the pure modules. If you need time in a pure module, add it as a parameter — never call `tokio::time::Instant::now()` directly inside `engine.rs` or `resume.rs`.

## Hamster D-Bus facts (verified against hamster 3.0.3 source)

Do not re-derive these from the D-Bus introspection — the API has edge cases:

- `GetTodaysFactsJSON` → JSON array of facts, **oldest-first**. `end == null` means the fact is currently running.
- `AddFactJSON` payload = fact shape minus `id`/`activity_id`, local `"YYYY-MM-DD HH:MM"` minute resolution.
  - Returns `0` for "same fact already on-going" — **this is a benign no-op, NOT a rejection error**.
  - Auto-closes/merges any previous open fact.
- **Always use `StopTracking(0)`** (server-side "now"). Non-zero timestamps hit a naive-UTC quirk in older hamster versions.

## Engine semantics

- **Starts are immediate.** No debounce on entering a tracked context.
- **Destructive transitions are debounced.** Leaving a tracked context (→ stop) and cross-entity switches both wait for the configured grace period before committing. A flap-back within the grace period cancels the pending transition.
- **Manual hamster changes win.** A `FactsChanged` D-Bus signal that matches a context-derived pending action is treated as a confirmation (the timer is cancelled, the transition is already done). A `FactsChanged` that contradicts a pending timer does NOT cancel it — manual changes to Hamster are respected, not overridden.
- **`set_running` reconcile:** after any hamster call, re-read the running fact to confirm the expected state. Divergence (e.g. the user manually stopped hamster mid-timer) resets the engine to idle.

## Conventions

- No design-doc/phase references in code or commit messages (hooks enforce this).
- Commit trailers per workspace CLAUDE.md: `Review-Needed: committed by Claude Code` + `AI-Assisted: <mode> (<model-id>)`.
- Commits in this repo are self-contained.
