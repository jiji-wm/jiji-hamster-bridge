//! Hysteresis state machine: desktop-context transitions in, hamster commands out.

use std::time::Duration;

use tokio::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Tracked(String),
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Stop the running fact (server-side "now").
    Stop,
    /// Start tracking from an untracked state (resume-or-placeholder).
    Start { entity: String },
    /// Stop the running fact and start `entity` at the same boundary.
    Switch { entity: String },
}

#[derive(Debug, Clone, Copy)]
pub struct Timings {
    pub switch_immediate: bool,
    pub return_debounce: Duration,
    pub untracked_grace: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingAction {
    Pause,
    Switch(String),
}

#[derive(Debug)]
struct Pending {
    action: PendingAction,
    deadline: Instant,
}

#[derive(Debug)]
pub struct Engine {
    timings: Timings,
    /// Entity the bridge believes is currently tracked.
    running: Option<String>,
    /// Entity that was running before the last switch (flap-back anchor).
    previous: Option<String>,
    last_switch_at: Option<Instant>,
    pending: Option<Pending>,
}

impl Engine {
    pub fn new(timings: Timings) -> Self {
        Self {
            timings,
            running: None,
            previous: None,
            last_switch_at: None,
            pending: None,
        }
    }

    /// Does not retune an in-flight deadline; it stays anchored to the timings
    /// under which it was scheduled.
    pub fn set_timings(&mut self, timings: Timings) {
        self.timings = timings;
    }

    /// Reconcile with hamster reality (FactsChanged / startup). Manual
    /// changes win: adopt the new state and drop all hysteresis history.
    ///
    /// A reconcile that merely confirms the current belief is a no-op by
    /// design: FactsChanged also fires for description/past-fact edits and
    /// as the async echo of this bridge's own commands — none of those say
    /// anything about desktop context, so in-flight timers survive.
    pub fn set_running(&mut self, entity: Option<String>) {
        if self.running != entity {
            self.running = entity;
            self.previous = None;
            self.last_switch_at = None;
            self.pending = None;
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|p| p.deadline)
    }

    pub fn on_context(&mut self, target: Target, now: Instant) -> Vec<Command> {
        match target {
            Target::Tracked(e) => self.on_tracked(e, now),
            Target::Untracked => self.on_untracked(now),
        }
    }

    fn on_tracked(&mut self, e: String, now: Instant) -> Vec<Command> {
        if self.running.as_deref() == Some(&e) {
            // includes "returned within the grace period": cancel the pause
            self.pending = None;
            return vec![];
        }
        // keep an already-pending switch to the same entity (don't reset its deadline)
        if let Some(p) = &self.pending
            && p.action == PendingAction::Switch(e.clone())
        {
            return vec![];
        }
        self.pending = None;
        if self.running.is_none() {
            self.running = Some(e.clone());
            self.previous = None;
            self.last_switch_at = None;
            return vec![Command::Start { entity: e }];
        }
        // running some other entity
        // An untracked excursion replaces a pending Switch with a Pause, so returning re-arms the
        // flap-back window from "now" — each return restarts the proof-of-intent clock.
        if self.debounce_switch_to(&e, now) {
            self.pending = Some(Pending {
                action: PendingAction::Switch(e),
                deadline: now + self.timings.return_debounce,
            });
            return vec![];
        }
        self.do_switch(e, now)
    }

    fn on_untracked(&mut self, now: Instant) -> Vec<Command> {
        if self.running.is_none() {
            self.pending = None;
            return vec![];
        }
        // keep an existing pause deadline; replace a pending switch
        if !matches!(
            self.pending,
            Some(Pending {
                action: PendingAction::Pause,
                ..
            })
        ) {
            self.pending = Some(Pending {
                action: PendingAction::Pause,
                deadline: now + self.timings.untracked_grace,
            });
        }
        vec![]
    }

    pub fn on_tick(&mut self, now: Instant) -> Vec<Command> {
        let Some(p) = &self.pending else {
            return vec![];
        };
        // Fires on now >= deadline. No backdating: the boundary is the caller's "now" at apply
        // time, so time spent inside a pending window stays attributed to the still-running fact.
        if now < p.deadline {
            return vec![];
        }
        let action = self.pending.take().unwrap().action;
        match action {
            PendingAction::Pause => {
                self.running = None;
                vec![Command::Stop]
            }
            PendingAction::Switch(e) => self.do_switch(e, now),
        }
    }

    fn do_switch(&mut self, e: String, now: Instant) -> Vec<Command> {
        self.previous = self.running.replace(e.clone());
        self.last_switch_at = Some(now);
        vec![Command::Switch { entity: e }]
    }

    /// Flap-back rule: debounce a switch back to the previously running
    /// entity shortly after the last switch — or every switch when
    /// switch_immediate is off.
    fn debounce_switch_to(&self, e: &str, now: Instant) -> bool {
        if !self.timings.switch_immediate {
            return true;
        }
        self.previous.as_deref() == Some(e)
            && self
                .last_switch_at
                .is_some_and(|t| now.duration_since(t) < self.timings.return_debounce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::Instant;

    fn engine() -> Engine {
        Engine::new(Timings {
            switch_immediate: true,
            return_debounce: Duration::from_secs(60),
            untracked_grace: Duration::from_secs(60),
        })
    }

    fn tracked(e: &str) -> Target {
        Target::Tracked(e.to_string())
    }

    #[tokio::test(start_paused = true)]
    async fn untracked_to_tracked_starts_immediately() {
        let mut en = engine();
        let cmds = en.on_context(tracked("work1"), Instant::now());
        assert_eq!(
            cmds,
            vec![Command::Start {
                entity: "work1".into()
            }]
        );
        assert_eq!(en.next_deadline(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn same_entity_is_noop() {
        let mut en = engine();
        en.on_context(tracked("work1"), Instant::now());
        let cmds = en.on_context(tracked("work1"), Instant::now());
        assert!(cmds.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn tracked_to_different_tracked_switches_immediately() {
        let mut en = engine();
        en.on_context(tracked("work1"), Instant::now());
        let cmds = en.on_context(tracked("work2"), Instant::now());
        assert_eq!(
            cmds,
            vec![Command::Switch {
                entity: "work2".into()
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn untracked_with_nothing_running_is_noop() {
        let mut en = engine();
        let cmds = en.on_context(Target::Untracked, Instant::now());
        assert!(cmds.is_empty());
        assert_eq!(en.next_deadline(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn set_running_reconciles_manual_changes() {
        let mut en = engine();
        en.on_context(tracked("work1"), Instant::now());
        // user manually stopped tracking in the hamster GUI
        en.set_running(None);
        // re-entering work1 therefore starts (not a no-op)
        let cmds = en.on_context(tracked("work1"), Instant::now());
        assert_eq!(
            cmds,
            vec![Command::Start {
                entity: "work1".into()
            }]
        );
        // user manually started work2; bridge view follows, no commands
        en.set_running(Some("work2".into()));
        assert!(en.on_context(tracked("work2"), Instant::now()).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn untracked_schedules_pause_and_fires_after_grace() {
        let mut en = engine();
        en.on_context(tracked("work1"), Instant::now());
        let cmds = en.on_context(Target::Untracked, Instant::now());
        assert!(cmds.is_empty()); // fact keeps running
        let deadline = en.next_deadline().unwrap();
        // before the deadline: nothing
        assert!(en.on_tick(deadline - Duration::from_secs(1)).is_empty());
        // at the deadline: stop
        assert_eq!(en.on_tick(deadline), vec![Command::Stop]);
        assert_eq!(en.next_deadline(), None);
        // settled untracked afterwards is a no-op
        assert!(en.on_context(Target::Untracked, Instant::now()).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn returning_within_grace_cancels_pause_with_no_interruption() {
        let mut en = engine();
        en.on_context(tracked("work1"), Instant::now());
        en.on_context(Target::Untracked, Instant::now());
        let cmds = en.on_context(tracked("work1"), Instant::now());
        assert!(cmds.is_empty()); // no stop, no start — fact never stopped
        assert_eq!(en.next_deadline(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn bouncing_between_untracked_keeps_original_deadline() {
        let mut en = engine();
        let t0 = Instant::now();
        en.on_context(tracked("work1"), t0);
        en.on_context(Target::Untracked, t0);
        let d1 = en.next_deadline().unwrap();
        // wandering through another untracked context 30s later
        en.on_context(Target::Untracked, t0 + Duration::from_secs(30));
        assert_eq!(en.next_deadline().unwrap(), d1); // not extended
    }

    #[tokio::test(start_paused = true)]
    async fn pending_pause_to_other_tracked_switches_with_no_untracked_interval() {
        let mut en = engine();
        en.on_context(tracked("work1"), Instant::now());
        en.on_context(Target::Untracked, Instant::now());
        let cmds = en.on_context(tracked("work2"), Instant::now());
        // Switch (not Stop+Start): the boundary is shared, no untracked gap
        assert_eq!(
            cmds,
            vec![Command::Switch {
                entity: "work2".into()
            }]
        );
        assert_eq!(en.next_deadline(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn flap_back_to_previous_is_debounced_then_fires() {
        let mut en = engine();
        let t0 = Instant::now();
        en.on_context(tracked("work1"), t0);
        en.on_context(tracked("work2"), t0 + Duration::from_secs(5)); // switch
        // back to work1 10s later: previous + within window -> pending, no command
        let cmds = en.on_context(tracked("work1"), t0 + Duration::from_secs(15));
        assert!(cmds.is_empty());
        let deadline = en.next_deadline().unwrap();
        // staying on work1 (more context events) must not extend the deadline
        en.on_context(tracked("work1"), t0 + Duration::from_secs(30));
        assert_eq!(en.next_deadline().unwrap(), deadline);
        // deadline passes -> the switch fires
        assert_eq!(
            en.on_tick(deadline),
            vec![Command::Switch {
                entity: "work1".into()
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn flap_back_cancelled_by_leaving_again() {
        let mut en = engine();
        let t0 = Instant::now();
        en.on_context(tracked("work1"), t0);
        en.on_context(tracked("work2"), t0 + Duration::from_secs(5));
        en.on_context(tracked("work1"), t0 + Duration::from_secs(15)); // pending flap-back
        // user bounces back to work2 before the deadline: pending dropped, no-op
        let cmds = en.on_context(tracked("work2"), t0 + Duration::from_secs(20));
        assert!(cmds.is_empty());
        assert_eq!(en.next_deadline(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn return_after_window_switches_immediately() {
        let mut en = engine();
        let t0 = Instant::now();
        en.on_context(tracked("work1"), t0);
        en.on_context(tracked("work2"), t0 + Duration::from_secs(5));
        let cmds = en.on_context(tracked("work1"), t0 + Duration::from_secs(120));
        assert_eq!(
            cmds,
            vec![Command::Switch {
                entity: "work1".into()
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn third_entity_is_never_debounced() {
        let mut en = engine();
        let t0 = Instant::now();
        en.on_context(tracked("work1"), t0);
        en.on_context(tracked("work2"), t0 + Duration::from_secs(5));
        let cmds = en.on_context(tracked("work3"), t0 + Duration::from_secs(10));
        assert_eq!(
            cmds,
            vec![Command::Switch {
                entity: "work3".into()
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn switch_immediate_false_debounces_every_switch() {
        let mut en = Engine::new(Timings {
            switch_immediate: false,
            return_debounce: Duration::from_secs(60),
            untracked_grace: Duration::from_secs(60),
        });
        let t0 = Instant::now();
        en.on_context(tracked("work1"), t0); // start is still immediate
        let cmds = en.on_context(tracked("work2"), t0 + Duration::from_secs(5));
        assert!(cmds.is_empty());
        assert!(en.next_deadline().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn confirming_reconcile_keeps_pending_pause() {
        let mut en = engine();
        en.on_context(tracked("work1"), Instant::now());
        en.on_context(Target::Untracked, Instant::now());
        let deadline = en.next_deadline().unwrap();
        // FactsChanged echo confirming the running fact: timers must survive
        en.set_running(Some("work1".into()));
        assert_eq!(en.next_deadline(), Some(deadline));
        assert_eq!(en.on_tick(deadline), vec![Command::Stop]);
    }

    #[tokio::test(start_paused = true)]
    async fn changing_reconcile_clears_pending() {
        let mut en = engine();
        let t0 = Instant::now();
        en.on_context(tracked("work1"), t0);
        en.on_context(tracked("work2"), t0 + Duration::from_secs(5));
        en.on_context(tracked("work1"), t0 + Duration::from_secs(10)); // pending flap-back
        assert!(en.next_deadline().is_some());
        // user manually started something else: adopt + drop the timer
        en.set_running(Some("work3".into()));
        assert_eq!(en.next_deadline(), None);
        assert!(en.on_tick(t0 + Duration::from_secs(120)).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn previous_tracks_immediately_prior_entity_across_chain() {
        let mut en = engine();
        let t0 = Instant::now();
        en.on_context(tracked("work1"), t0);
        en.on_context(tracked("work2"), t0 + Duration::from_secs(5));
        en.on_context(tracked("work3"), t0 + Duration::from_secs(10));
        // flap-back anchor is work2 (immediately prior), not work1:
        // returning to work1 within the window switches immediately...
        let cmds = en.on_context(tracked("work1"), t0 + Duration::from_secs(15));
        assert_eq!(
            cmds,
            vec![Command::Switch {
                entity: "work1".into()
            }]
        );
    }
}
