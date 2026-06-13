//! The daemon loop: contexts in, hamster calls out.

use tokio::sync::mpsc::Receiver;

use crate::config::Config;
use crate::engine::{Command, Engine, Target, Timings};
use crate::events::Tracker;
use crate::hamster::{HamsterClient, entity_of, running_fact};
use crate::resume::{now_local_string, plan_resume};

/// Inputs multiplexed into the loop by the binary (or by tests).
#[derive(Debug)]
pub enum LoopInput {
    /// One line from `jiji msg --json event-stream`.
    EventLine(String),
    /// The event-stream subprocess died; state will be rebuilt on reconnect.
    StreamReset,
    /// hamster's FactsChanged signal fired.
    FactsChanged,
    /// A new validated config (file watcher / SIGHUP).
    ConfigReloaded(Config),
}

fn timings(cfg: &Config) -> Timings {
    Timings {
        switch_immediate: cfg.defaults.switch_immediate,
        return_debounce: std::time::Duration::from_secs(cfg.defaults.return_debounce_secs),
        untracked_grace: std::time::Duration::from_secs(cfg.defaults.untracked_grace_secs),
    }
}

pub async fn run_loop<C: HamsterClient>(
    mut inputs: Receiver<LoopInput>,
    client: C,
    mut cfg: Config,
) {
    let mut tracker = Tracker::default();
    let mut engine = Engine::new(timings(&cfg));
    // startup reconcile: adopt hamster's current state before acting
    reconcile(&mut engine, &client, &cfg).await;

    loop {
        let deadline = engine.next_deadline();
        let input = tokio::select! {
            i = inputs.recv() => match i {
                Some(i) => Some(i),
                None => break, // all senders gone: shut down
            },
            _ = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => None,
        };

        let now = tokio::time::Instant::now();
        let cmds = match input {
            None => engine.on_tick(now),
            Some(LoopInput::EventLine(line)) => {
                if tracker.apply_line(&line) {
                    engine.on_context(target_of(&cfg, &tracker), now)
                } else {
                    vec![]
                }
            }
            Some(LoopInput::StreamReset) => {
                tracker = Tracker::default();
                vec![]
            }
            Some(LoopInput::FactsChanged) => {
                reconcile(&mut engine, &client, &cfg).await;
                vec![]
            }
            Some(LoopInput::ConfigReloaded(new_cfg)) => {
                cfg = new_cfg;
                engine.set_timings(timings(&cfg));
                // re-evaluate once through the normal machinery
                engine.on_context(target_of(&cfg, &tracker), now)
            }
        };

        for cmd in cmds {
            if let Err(e) = execute(&cmd, &client, &cfg).await {
                log::error!("executing {cmd:?}: {e:#}");
                client.notify(&format!("hamster call failed: {e:#}")).await;
            }
        }
    }
}

fn target_of(cfg: &Config, tracker: &Tracker) -> Target {
    match cfg.effective(&tracker.context()) {
        Some(rule) => Target::Tracked(rule.entity),
        None => Target::Untracked,
    }
}

async fn execute<C: HamsterClient>(cmd: &Command, client: &C, cfg: &Config) -> anyhow::Result<()> {
    match cmd {
        Command::Stop => client.stop_tracking().await,
        // Switch stops explicitly so the boundary is shared. Start (engine
        // believes nothing runs) deliberately does NOT stop: a manually
        // started fact must win, and hamster itself auto-closes/merges a
        // previous open fact on add (returning 0 for an identical one).
        Command::Start { entity } | Command::Switch { entity } => {
            // resolve the rule at execution time (config may have reloaded)
            let rule = cfg
                .rule_for_entity(entity)
                .ok_or_else(|| anyhow::anyhow!("no rule resolves entity '{entity}'"))?;
            let recent = client
                .recent_facts(cfg.defaults.resume_lookback_days)
                .await?;
            let plan = plan_resume(&recent, &rule, &cfg.defaults, &now_local_string());
            if matches!(cmd, Command::Switch { .. }) {
                client.stop_tracking().await?;
            }
            client.add_fact(&plan.fact).await?;
            if let Some(msg) = plan.notification {
                client.notify(&msg).await;
            }
            Ok(())
        }
    }
}

async fn reconcile<C: HamsterClient>(engine: &mut Engine, client: &C, cfg: &Config) {
    match client.todays_facts().await {
        Ok(facts) => {
            let entity =
                running_fact(&facts).and_then(|f| entity_of(f, &cfg.defaults.entity_tag_key));
            engine.set_running(entity);
        }
        Err(e) => log::warn!("reconcile: cannot read hamster facts: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::mpsc;

    use crate::config::Config;
    use crate::hamster::{Fact, HamsterClient, NewFact};

    /// Recording fake: one scripted fact list serves both fact queries; records every call.
    #[derive(Clone, Default)]
    struct FakeHamster {
        facts: Arc<Mutex<Vec<Fact>>>,
        calls: Arc<Mutex<Vec<String>>>,
        fail_next_add: Arc<Mutex<bool>>,
    }

    impl HamsterClient for FakeHamster {
        async fn todays_facts(&self) -> anyhow::Result<Vec<Fact>> {
            Ok(self.facts.lock().unwrap().clone())
        }
        async fn recent_facts(&self, days: u64) -> anyhow::Result<Vec<Fact>> {
            self.calls.lock().unwrap().push(format!("recent:{days}"));
            Ok(self.facts.lock().unwrap().clone())
        }
        async fn add_fact(&self, fact: &NewFact) -> anyhow::Result<()> {
            if std::mem::take(&mut *self.fail_next_add.lock().unwrap()) {
                self.calls.lock().unwrap().push("add-failed".into());
                anyhow::bail!("simulated add failure");
            }
            self.calls
                .lock()
                .unwrap()
                .push(format!("add:{}@{}", fact.activity, fact.category));
            Ok(())
        }
        async fn stop_tracking(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("stop".into());
            Ok(())
        }
        async fn notify(&self, message: &str) {
            self.calls.lock().unwrap().push(format!("notify:{message}"));
        }
    }

    const CONFIG: &str = r#"
        [activities.work1]
        category = "work1.example"
        [activities.work2]
        category = "work2.example"
    "#;

    fn activity_switch_lines() -> Vec<String> {
        vec![
            // snapshot: work1 active, no focused named workspace
            r#"{"ActivitiesChanged":{"activities":[
                {"id":3,"name":"work1","is_active":true},
                {"id":6,"name":"games","is_active":false},
                {"id":8,"name":"work2","is_active":false}]}}"#
                .into(),
        ]
    }

    #[tokio::test(start_paused = true)]
    async fn full_cycle_track_pause_resume() {
        let fake = FakeHamster::default();
        let cfg = Config::parse(CONFIG).unwrap();
        let (tx, rx) = mpsc::channel(64);
        let handle = tokio::spawn(run_loop(rx, fake.clone(), cfg));

        // snapshot puts us in work1 -> placeholder started (no facts today)
        for l in activity_switch_lines() {
            tx.send(LoopInput::EventLine(l)).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        // switch to games (untracked): grace period, nothing yet
        tx.send(LoopInput::EventLine(
            r#"{"ActivitySwitched":{"id":6,"previous_id":3}}"#.into(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await; // < grace
        {
            let calls = fake.calls.lock().unwrap();
            assert_eq!(calls.iter().filter(|c| *c == "stop").count(), 0);
            assert!(
                calls
                    .iter()
                    .any(|c| c.starts_with("add:placeholder@work1.example"))
            );
        }
        // grace elapses -> stop fires
        tokio::time::sleep(Duration::from_secs(70)).await;
        assert!(fake.calls.lock().unwrap().iter().any(|c| c == "stop"));

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn tracked_to_tracked_switch_stops_then_adds() {
        let fake = FakeHamster::default();
        let cfg = Config::parse(CONFIG).unwrap();
        let (tx, rx) = mpsc::channel(64);
        let handle = tokio::spawn(run_loop(rx, fake.clone(), cfg));

        for l in activity_switch_lines() {
            tx.send(LoopInput::EventLine(l)).await.unwrap();
        }
        tx.send(LoopInput::EventLine(
            r#"{"ActivitySwitched":{"id":8,"previous_id":3}}"#.into(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let calls = fake.calls.lock().unwrap().clone();
        // switch = stop + add, in that order
        let stop_idx = calls.iter().position(|c| c == "stop").unwrap();
        let add_idx = calls
            .iter()
            .position(|c| c.starts_with("add:placeholder@work2.example"))
            .unwrap();
        assert!(stop_idx < add_idx);

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn resume_clones_a_prior_fact_within_the_lookback_window() {
        // a completed work2 fact from earlier in the window (not today) exists;
        // entering work2 must clone it via the recent-facts query rather than
        // minting a placeholder
        let fake = FakeHamster::default();
        *fake.facts.lock().unwrap() = vec![
            serde_json::from_value(serde_json::json!({
                "activity": "devel", "category": "work2.example",
                "description": "ongoing work", "tags": ["entity: work2"], "id": 7,
                "range": {"start": "2026-06-02 09:00", "end": "2026-06-02 17:00"},
            }))
            .unwrap(),
        ];
        let cfg = Config::parse(CONFIG).unwrap();
        let (tx, rx) = mpsc::channel(64);
        let handle = tokio::spawn(run_loop(rx, fake.clone(), cfg));

        for l in activity_switch_lines() {
            tx.send(LoopInput::EventLine(l)).await.unwrap();
        }
        tx.send(LoopInput::EventLine(
            r#"{"ActivitySwitched":{"id":8,"previous_id":3}}"#.into(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;

        let calls = fake.calls.lock().unwrap().clone();
        // the prior fact is cloned, not a placeholder
        assert!(calls.iter().any(|c| c == "add:devel@work2.example"));
        assert!(!calls.iter().any(|c| c.starts_with("add:placeholder@work2")));
        // the resume path queried the configured (default 5-day) window
        assert!(calls.iter().any(|c| c == "recent:5"));

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn failed_add_heals_via_facts_changed_reconcile() {
        let fake = FakeHamster::default();
        *fake.fail_next_add.lock().unwrap() = true;
        let cfg = Config::parse(CONFIG).unwrap();
        let (tx, rx) = mpsc::channel(64);
        let handle = tokio::spawn(run_loop(rx, fake.clone(), cfg));

        // entering work1: the placeholder add fails -> engine believes
        // running=work1, hamster actually has nothing
        for l in activity_switch_lines() {
            tx.send(LoopInput::EventLine(l)).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(fake.calls.lock().unwrap().iter().any(|c| c == "add-failed"));

        // hamster's FactsChanged (empty facts) heals the engine belief
        tx.send(LoopInput::FactsChanged).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;

        // next context event starts fresh (Start, not a no-op):
        tx.send(LoopInput::EventLine(
            r#"{"ActivitySwitched":{"id":8,"previous_id":3}}"#.into(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let calls = fake.calls.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("add:placeholder@work2.example"))
        );
        // Start path: no explicit stop was issued (nothing was running)
        assert_eq!(calls.iter().filter(|c| *c == "stop").count(), 0);

        drop(tx);
        handle.await.unwrap();
    }
}
