//! Resume planning: clone the newest recent entity-tagged fact, or start a placeholder.

use crate::config::{Defaults, TrackedRule};
use crate::hamster::{Fact, NewFact, NewRange, entity_of};

pub struct ResumePlan {
    pub fact: NewFact,
    pub notification: Option<String>,
}

/// `now_local` is a hamster-format local timestamp ("YYYY-MM-DD HH:MM").
///
/// `facts` is the recent fact window (see [`day_range`]) the planner searches,
/// newest-first, for a prior fact of the same entity to clone. Pure planning
/// only: the returned fact is open-ended (`end: None`), so the caller must stop
/// any currently running fact before submitting it — including when the matched
/// template fact is itself still running.
pub fn plan_resume(
    facts: &[Fact],
    rule: &TrackedRule,
    defaults: &Defaults,
    now_local: &str,
) -> ResumePlan {
    let found = facts
        .iter()
        .rev()
        .find(|f| entity_of(f, &defaults.entity_tag_key).as_deref() == Some(&rule.entity));
    match found {
        Some(f) => ResumePlan {
            fact: NewFact {
                activity: f.activity.clone(),
                category: f.category.clone(),
                description: carried_description(f, defaults, now_local),
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

/// The description to carry forward when cloning `f`. When continuation marking
/// is enabled and `f` started on the same calendar day as `now_local`, each
/// carried-over block's first line is stamped with the continued marker (`..`);
/// otherwise the description is cloned verbatim. A cross-day clone (or a fact
/// with no recorded start) is a fresh effort today and is never marked.
fn carried_description(f: &Fact, defaults: &Defaults, now_local: &str) -> String {
    let same_day = f
        .range
        .start
        .as_deref()
        .is_some_and(|start| date_part(start) == date_part(now_local));
    if defaults.mark_continuations && same_day {
        crate::markers::mark_continuation(&f.description)
    } else {
        f.description.clone()
    }
}

/// The "YYYY-MM-DD" date token of a hamster-format "YYYY-MM-DD HH:MM" timestamp.
fn date_part(ts: &str) -> &str {
    ts.split_whitespace().next().unwrap_or(ts)
}

/// Hamster-format local "now" ("YYYY-MM-DD HH:MM").
pub fn now_local_string() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

/// Hamster `GetFactsJSON` range string spanning the last `days` days: from
/// `days` days before `today` through `today`, inclusive. `days == 0` yields a
/// single-day (today-only) range. Pure: the clock is supplied via `today`.
pub fn day_range(today: chrono::NaiveDate, days: u64) -> String {
    let start = today - chrono::Duration::days(days as i64);
    format!("{} {}", start.format("%Y-%m-%d"), today.format("%Y-%m-%d"))
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
        // same-day clone → the carried description is stamped continued
        assert_eq!(plan.fact.description, ".. work notes");
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

    #[test]
    fn resumes_a_fact_from_earlier_in_the_window() {
        // a matching fact several days old (not today) is still cloned, not
        // replaced by a placeholder
        let facts = vec![fact("devel", &["entity: work2"], Some("2026-06-02 11:00"))];
        let plan = plan_resume(&facts, &rule(), &Defaults::default(), NOW);
        assert_eq!(plan.fact.activity, "devel");
        assert!(plan.notification.is_none());
    }

    fn fact_started(activity: &str, tags: &[&str], start: Option<&str>) -> Fact {
        serde_json::from_value(serde_json::json!({
            "activity": activity, "category": "work2.example",
            "description": "write report\n- draft intro", "tags": tags, "id": 1,
            "range": {"start": start, "end": "2026-06-06 10:00"},
        }))
        .unwrap()
    }

    #[test]
    fn same_day_clone_marks_carried_block_first_lines() {
        let facts = vec![fact_started(
            "task",
            &["entity: work2"],
            Some("2026-06-06 09:00"),
        )];
        let plan = plan_resume(&facts, &rule(), &Defaults::default(), NOW);
        // first line marked, bullet untouched
        assert_eq!(plan.fact.description, ".. write report\n- draft intro");
    }

    #[test]
    fn cross_day_clone_leaves_description_verbatim() {
        // a fact started on a previous day is a fresh block today, not a
        // continuation — never stamped.
        let facts = vec![fact_started(
            "task",
            &["entity: work2"],
            Some("2026-06-05 09:00"),
        )];
        let plan = plan_resume(&facts, &rule(), &Defaults::default(), NOW);
        assert_eq!(plan.fact.description, "write report\n- draft intro");
    }

    #[test]
    fn null_start_is_not_treated_as_same_day() {
        let facts = vec![fact_started("task", &["entity: work2"], None)];
        let plan = plan_resume(&facts, &rule(), &Defaults::default(), NOW);
        assert_eq!(plan.fact.description, "write report\n- draft intro");
    }

    #[test]
    fn disabled_flag_leaves_description_verbatim_even_same_day() {
        let d = Defaults {
            mark_continuations: false,
            ..Defaults::default()
        };
        let facts = vec![fact_started(
            "task",
            &["entity: work2"],
            Some("2026-06-06 09:00"),
        )];
        let plan = plan_resume(&facts, &rule(), &d, NOW);
        assert_eq!(plan.fact.description, "write report\n- draft intro");
    }

    #[test]
    fn placeholder_description_is_never_marked() {
        let plan = plan_resume(&[], &rule(), &Defaults::default(), NOW);
        assert_eq!(plan.fact.description, "auto — rename me");
    }

    #[test]
    fn day_range_spans_lookback_window_inclusive() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 6, 13).unwrap();
        assert_eq!(day_range(today, 5), "2026-06-08 2026-06-13");
        // 0 = today only
        assert_eq!(day_range(today, 0), "2026-06-13 2026-06-13");
        // crosses a month boundary
        assert_eq!(day_range(today, 20), "2026-05-24 2026-06-13");
    }
}
