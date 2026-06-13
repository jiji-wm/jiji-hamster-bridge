//! Hamster D-Bus fact types and the client trait the engine executor uses.

use std::future::Future;

use serde::{Deserialize, Serialize};

/// A fact as returned by `GetTodaysFactsJSON` (hamster 3.0.3 shape).
#[derive(Debug, Clone, Deserialize)]
pub struct Fact {
    pub activity: String,
    pub category: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub id: i64,
    pub range: FactRange,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FactRange {
    /// Can be null on the wire: hamster's to_dbus_fact_json emits None starts as JSON null.
    pub start: Option<String>,
    pub end: Option<String>,
}

impl Fact {
    pub fn is_running(&self) -> bool {
        self.range.end.is_none()
    }
}

/// Payload for `AddFactJSON`: same shape minus `id`/`activity_id`
/// (the service rebuilds those). Datetimes are local "YYYY-MM-DD HH:MM".
#[derive(Debug, Clone, Serialize)]
pub struct NewFact {
    pub activity: String,
    pub category: String,
    pub description: String,
    pub tags: Vec<String>,
    pub range: NewRange,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewRange {
    pub start: String,
    pub end: Option<String>,
}

/// Extract the entity from a fact's tags: a tag `"<key>: <value>"`
/// (leading/trailing whitespace around the value is trimmed).
pub fn entity_of(fact: &Fact, tag_key: &str) -> Option<String> {
    let prefix = format!("{tag_key}:");
    fact.tags
        .iter()
        .find_map(|t| t.strip_prefix(&prefix))
        .map(|v| v.trim().to_string())
}

/// The currently running fact, if any (last open-ended fact of the day).
pub fn running_fact(facts: &[Fact]) -> Option<&Fact> {
    facts.iter().rev().find(|f| f.is_running())
}

/// Everything the bridge needs from hamster. `ZbusHamster` is the real
/// implementation; tests use a recording fake.
pub trait HamsterClient {
    fn todays_facts(&self) -> impl Future<Output = anyhow::Result<Vec<Fact>>> + Send;
    /// Facts from the last `days` days (today and the `days` preceding days),
    /// oldest-first. Used by the resume planner to clone a recent prior fact.
    fn recent_facts(&self, days: u64) -> impl Future<Output = anyhow::Result<Vec<Fact>>> + Send;
    fn add_fact(&self, fact: &NewFact) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn stop_tracking(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn notify(&self, message: &str) -> impl Future<Output = ()> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real shape from GetTodaysFactsJSON on hamster 3.0.3.
    const LIVE_FACT: &str = r#"{"activity": "support", "category": "work1.example",
        "description": "fixing a persistent cache bug",
        "tags": ["entity: work1", "location: home"],
        "id": 20534, "activity_id": 1260,
        "range": {"start": "2026-06-06 14:09", "end": null}}"#;

    #[test]
    fn parses_live_fact_shape() {
        let f: Fact = serde_json::from_str(LIVE_FACT).unwrap();
        assert_eq!(f.activity, "support");
        assert_eq!(f.category, "work1.example");
        assert_eq!(f.tags, vec!["entity: work1", "location: home"]);
        assert_eq!(f.range.start.as_deref(), Some("2026-06-06 14:09"));
        assert!(f.range.end.is_none());
        assert!(f.is_running());
    }

    #[test]
    fn finished_fact_is_not_running() {
        let mut f: Fact = serde_json::from_str(LIVE_FACT).unwrap();
        f.range.end = Some("2026-06-06 15:00".into());
        assert!(!f.is_running());
    }

    #[test]
    fn entity_of_matches_tag_key_with_and_without_space() {
        let f: Fact = serde_json::from_str(LIVE_FACT).unwrap();
        assert_eq!(entity_of(&f, "entity").as_deref(), Some("work1"));
        let mut g = f.clone();
        g.tags = vec!["entity:work2".into()];
        assert_eq!(entity_of(&g, "entity").as_deref(), Some("work2"));
        let mut h = f.clone();
        h.tags = vec!["location: home".into()];
        assert_eq!(entity_of(&h, "entity"), None);
    }

    #[test]
    fn running_fact_finds_last_open_fact() {
        let done: Fact = {
            let mut f: Fact = serde_json::from_str(LIVE_FACT).unwrap();
            f.range.end = Some("2026-06-06 12:00".into());
            f.id = 1;
            f
        };
        // An earlier open fact — should NOT be returned when a later open one exists.
        let earlier_open: Fact = {
            let mut f: Fact = serde_json::from_str(LIVE_FACT).unwrap();
            f.id = 2;
            f
        };
        let open: Fact = {
            let mut f: Fact = serde_json::from_str(LIVE_FACT).unwrap();
            f.id = 3;
            f
        };
        // done, then earlier_open, then open — running_fact must return open (id=3).
        let facts = vec![done.clone(), earlier_open, open.clone()];
        assert_eq!(running_fact(&facts).map(|f| f.id), Some(3));
        assert!(running_fact(&[done]).is_none());
    }

    #[test]
    fn new_fact_serializes_without_id_fields() {
        let nf = NewFact {
            activity: "placeholder".into(),
            category: "work2.example".into(),
            description: "auto".into(),
            tags: vec!["entity: work2".into()],
            range: NewRange {
                start: "2026-06-06 15:32".into(),
                end: None,
            },
        };
        let v: serde_json::Value = serde_json::to_value(&nf).unwrap();
        assert!(v.get("id").is_none());
        assert!(v.get("activity_id").is_none());
        assert_eq!(v["range"]["end"], serde_json::Value::Null);
    }
}
