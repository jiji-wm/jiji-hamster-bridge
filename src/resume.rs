//! Resume planning: clone today's newest entity-tagged fact, or start a placeholder.

use crate::config::{Defaults, TrackedRule};
use crate::hamster::{Fact, NewFact, NewRange, entity_of};

pub struct ResumePlan {
    pub fact: NewFact,
    pub notification: Option<String>,
}

/// `now_local` is a hamster-format local timestamp ("YYYY-MM-DD HH:MM").
///
/// Pure planning only: the returned fact is open-ended (`end: None`), so the
/// caller must stop any currently running fact before submitting it —
/// including when the matched template fact is itself still running.
pub fn plan_resume(
    todays: &[Fact],
    rule: &TrackedRule,
    defaults: &Defaults,
    now_local: &str,
) -> ResumePlan {
    let found = todays
        .iter()
        .rev()
        .find(|f| entity_of(f, &defaults.entity_tag_key).as_deref() == Some(&rule.entity));
    match found {
        Some(f) => ResumePlan {
            fact: NewFact {
                activity: f.activity.clone(),
                category: f.category.clone(),
                description: f.description.clone(),
                tags: f.tags.clone(),
                range: NewRange {
                    start: now_local.to_string(),
                    end: None,
                },
            },
            notification: None,
        },
        None => {
            let mut tags = vec![format!("{}: {}", defaults.entity_tag_key, rule.entity)];
            tags.extend(defaults.extra_tags.iter().cloned());
            ResumePlan {
                fact: NewFact {
                    activity: rule.placeholder_activity.clone(),
                    category: rule.category.clone(),
                    description: rule.placeholder_description.clone(),
                    tags,
                    range: NewRange {
                        start: now_local.to_string(),
                        end: None,
                    },
                },
                notification: Some(format!(
                    "started placeholder for {} — rename it in hamster when convenient",
                    rule.entity
                )),
            }
        }
    }
}

/// Hamster-format local "now" ("YYYY-MM-DD HH:MM").
pub fn now_local_string() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Defaults, TrackedRule};
    use crate::hamster::Fact;

    fn rule() -> TrackedRule {
        TrackedRule {
            entity: "work2".into(),
            category: "work2.example".into(),
            placeholder_activity: "placeholder".into(),
            placeholder_description: "auto — rename me".into(),
        }
    }

    fn fact(activity: &str, tags: &[&str], end: Option<&str>) -> Fact {
        serde_json::from_value(serde_json::json!({
            "activity": activity, "category": "work2.example",
            "description": "work notes", "tags": tags, "id": 1,
            "range": {"start": "2026-06-06 09:00", "end": end},
        }))
        .unwrap()
    }

    const NOW: &str = "2026-06-06 15:32";

    #[test]
    fn clones_newest_matching_fact() {
        let facts = vec![
            fact("old-task", &["entity: work2"], Some("2026-06-06 10:00")),
            fact(
                "newer-task",
                &["entity: work2", "location: home"],
                Some("2026-06-06 12:00"),
            ),
            fact("other", &["entity: work1"], Some("2026-06-06 13:00")),
        ];
        let plan = plan_resume(&facts, &rule(), &Defaults::default(), NOW);
        assert_eq!(plan.fact.activity, "newer-task");
        assert_eq!(plan.fact.description, "work notes");
        assert_eq!(plan.fact.tags, vec!["entity: work2", "location: home"]);
        assert_eq!(plan.fact.range.start, NOW);
        assert!(plan.fact.range.end.is_none());
        assert!(plan.notification.is_none());
    }

    #[test]
    fn no_match_creates_placeholder_with_notification() {
        let d = Defaults {
            extra_tags: vec!["location: home".into()],
            ..Defaults::default()
        };
        let facts = vec![fact("other", &["entity: work1"], Some("2026-06-06 13:00"))];
        let plan = plan_resume(&facts, &rule(), &d, NOW);
        assert_eq!(plan.fact.activity, "placeholder");
        assert_eq!(plan.fact.category, "work2.example");
        assert_eq!(plan.fact.description, "auto — rename me");
        assert_eq!(plan.fact.tags, vec!["entity: work2", "location: home"]);
        let msg = plan.notification.unwrap();
        assert!(msg.contains("work2"));
    }

    #[test]
    fn renamed_placeholder_teaches_subsequent_resumes() {
        // a corrected placeholder (renamed by the user) is found by tag
        let facts = vec![fact(
            "support",
            &["entity: work2", "location: home"],
            Some("2026-06-06 14:00"),
        )];
        let plan = plan_resume(&facts, &rule(), &Defaults::default(), NOW);
        assert_eq!(plan.fact.activity, "support");
        assert!(plan.notification.is_none());
    }

    #[test]
    fn empty_day_creates_placeholder() {
        let plan = plan_resume(&[], &rule(), &Defaults::default(), NOW);
        assert_eq!(plan.fact.activity, "placeholder");
        assert_eq!(plan.fact.tags, vec!["entity: work2"]);
        assert!(plan.notification.is_some());
    }

    #[test]
    fn clone_preserves_source_category() {
        let facts = vec![fact("task", &["entity: work2"], Some("2026-06-06 10:00"))];
        let plan = plan_resume(&facts, &rule(), &Defaults::default(), NOW);
        assert_eq!(plan.fact.category, "work2.example");
    }
}
