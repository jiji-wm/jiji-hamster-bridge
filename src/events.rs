//! jiji event-stream consumption: context tracking.

/// Desktop context: active activity name + focused workspace name (if named).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    pub activity: Option<String>,
    pub workspace: Option<String>,
}

use std::collections::HashMap;

use serde_json::Value;

/// Applies event-stream lines, maintaining activity + focused-workspace state.
/// Built from the on-connect snapshot events; deltas applied on top.
#[derive(Debug, Default)]
pub struct Tracker {
    /// activity id -> (name, is_active)
    activities: HashMap<u64, (String, bool)>,
    /// workspace id -> (name, is_focused)
    workspaces: HashMap<u64, (Option<String>, bool)>,
}

impl Tracker {
    pub fn context(&self) -> Context {
        Context {
            activity: self
                .activities
                .values()
                .find(|(_, active)| *active)
                .map(|(name, _)| name.clone()),
            workspace: self
                .workspaces
                .values()
                .find(|(_, focused)| *focused)
                .and_then(|(name, _)| name.clone()),
        }
    }

    /// Returns true when the line was a context-relevant event.
    pub fn apply_line(&mut self, line: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        let Some(obj) = v.as_object() else {
            return false;
        };
        let Some((kind, body)) = obj.iter().next() else {
            return false;
        };
        match kind.as_str() {
            "ActivitiesChanged" => {
                self.activities.clear();
                for a in body["activities"].as_array().into_iter().flatten() {
                    self.insert_activity(a);
                }
            }
            "ActivityCreated" => self.insert_activity(&body["activity"]),
            "ActivitySwitched" => {
                if let Some(id) = body["id"].as_u64() {
                    for (aid, (_, active)) in self.activities.iter_mut() {
                        *active = *aid == id;
                    }
                }
            }
            "ActivityRemoved" => {
                if let Some(id) = body["id"].as_u64() {
                    self.activities.remove(&id);
                }
            }
            "ActivityRenamed" => {
                if let (Some(id), Some(name)) = (body["id"].as_u64(), body["name"].as_str())
                    && let Some((n, _)) = self.activities.get_mut(&id)
                {
                    *n = name.to_string();
                }
            }
            "WorkspacesChanged" => {
                self.workspaces.clear();
                for w in body["workspaces"].as_array().into_iter().flatten() {
                    self.insert_workspace(w);
                }
            }
            "WorkspaceOpenedOrChanged" => self.insert_workspace(&body["workspace"]),
            "WorkspaceClosed" => {
                if let Some(id) = body["id"].as_u64() {
                    self.workspaces.remove(&id);
                }
            }
            "WorkspaceActivated" => {
                let (id, focused) = (body["id"].as_u64(), body["focused"].as_bool());
                if let (Some(id), Some(true)) = (id, focused) {
                    // Set all workspaces' focus to (wid == id); or_insert handles a workspace
                    // not in the map yet (WorkspaceActivated before its OpenedOrChanged).
                    for (wid, (_, f)) in self.workspaces.iter_mut() {
                        *f = *wid == id;
                    }
                    self.workspaces.entry(id).or_insert((None, true));
                }
            }
            _ => return false,
        }
        true
    }

    fn insert_activity(&mut self, a: &Value) {
        if let (Some(id), Some(name)) = (a["id"].as_u64(), a["name"].as_str()) {
            let is_active = a["is_active"].as_bool().unwrap_or(false);
            if is_active {
                for (_, active) in self.activities.values_mut() {
                    *active = false;
                }
            }
            self.activities.insert(id, (name.to_string(), is_active));
        }
    }

    fn insert_workspace(&mut self, w: &Value) {
        if let Some(id) = w["id"].as_u64() {
            let name = w["name"].as_str().map(String::from);
            let is_focused = w["is_focused"].as_bool().unwrap_or(false);
            if is_focused {
                for (_, f) in self.workspaces.values_mut() {
                    *f = false;
                }
            }
            self.workspaces.insert(id, (name, is_focused));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut Tracker, lines: &[&str]) {
        for l in lines {
            t.apply_line(l);
        }
    }

    const SNAPSHOT: &[&str] = &[
        r#"{"ActivitiesChanged":{"activities":[
            {"id":3,"name":"work1","is_config_declared":true,"is_active":false,"is_urgent":false,"last_active_seq":0},
            {"id":4,"name":"niri","is_config_declared":true,"is_active":true,"is_urgent":false,"last_active_seq":0},
            {"id":8,"name":"work2","is_config_declared":true,"is_active":false,"is_urgent":false,"last_active_seq":0}]}}"#,
        r#"{"WorkspacesChanged":{"workspaces":[
            {"id":14,"idx":2,"name":null,"output":"DP-3","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":194,"activities":[4],"is_sticky":false,"is_in_active_activity":true},
            {"id":9,"idx":0,"name":"invoicing","output":"DP-3","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":18,"activities":[3],"is_sticky":false,"is_in_active_activity":false}]}}"#,
    ];

    #[test]
    fn snapshot_builds_context() {
        let mut t = Tracker::default();
        feed(&mut t, SNAPSHOT);
        let c = t.context();
        assert_eq!(c.activity.as_deref(), Some("niri"));
        assert_eq!(c.workspace, None); // focused workspace is unnamed
    }

    #[test]
    fn activity_switch_updates_active() {
        let mut t = Tracker::default();
        feed(&mut t, SNAPSHOT);
        t.apply_line(r#"{"ActivitySwitched":{"id":3,"previous_id":4}}"#);
        assert_eq!(t.context().activity.as_deref(), Some("work1"));
    }

    #[test]
    fn workspace_activated_moves_focus() {
        let mut t = Tracker::default();
        feed(&mut t, SNAPSHOT);
        t.apply_line(r#"{"WorkspaceActivated":{"id":9,"focused":true}}"#);
        assert_eq!(t.context().workspace.as_deref(), Some("invoicing"));
        // focused=false (other-output activation) must NOT steal focus
        t.apply_line(r#"{"WorkspaceActivated":{"id":14,"focused":false}}"#);
        assert_eq!(t.context().workspace.as_deref(), Some("invoicing"));
    }

    #[test]
    fn workspace_opened_or_changed_upserts_and_refocuses() {
        let mut t = Tracker::default();
        feed(&mut t, SNAPSHOT);
        t.apply_line(
            r#"{"WorkspaceOpenedOrChanged":{"workspace":{"id":30,"idx":5,"name":"mail","output":"DP-3","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":null,"activities":[4],"is_sticky":false,"is_in_active_activity":true}}}"#,
        );
        assert_eq!(t.context().workspace.as_deref(), Some("mail"));
    }

    #[test]
    fn rename_remove_create_lifecycle() {
        let mut t = Tracker::default();
        feed(&mut t, SNAPSHOT);
        t.apply_line(r#"{"ActivityRenamed":{"id":4,"name":"compositor"}}"#);
        assert_eq!(t.context().activity.as_deref(), Some("compositor"));
        t.apply_line(
            r#"{"ActivityCreated":{"activity":{"id":9,"name":"new","is_config_declared":false,"is_active":true,"is_urgent":false,"last_active_seq":0}}}"#,
        );
        assert_eq!(t.context().activity.as_deref(), Some("new"));
        t.apply_line(r#"{"ActivityRemoved":{"id":9}}"#);
        assert_eq!(t.context().activity, None);
        t.apply_line(r#"{"WorkspaceClosed":{"id":9}}"#);
        assert!(t.context().workspace.is_none());
    }

    #[test]
    fn irrelevant_and_garbage_lines_are_ignored() {
        let mut t = Tracker::default();
        feed(&mut t, SNAPSHOT);
        let before = t.context();
        assert!(!t.apply_line(r#"{"ConfigLoaded":{"failed":false}}"#));
        assert!(!t.apply_line("not json"));
        assert_eq!(t.context(), before);
    }

    #[test]
    fn activity_switched_malformed_id_leaves_state_unchanged() {
        let mut t = Tracker::default();
        feed(&mut t, SNAPSHOT);
        t.apply_line(r#"{"ActivitySwitched":{"previous_id":4}}"#);
        assert_eq!(t.context().activity.as_deref(), Some("niri"));
    }

    #[test]
    fn activity_switched_to_unknown_id_clears_active() {
        let mut t = Tracker::default();
        feed(&mut t, SNAPSHOT);
        t.apply_line(r#"{"ActivitySwitched":{"id":999,"previous_id":4}}"#);
        assert_eq!(t.context().activity, None);
    }

    #[test]
    fn activity_renamed_unknown_id_is_ignored() {
        let mut t = Tracker::default();
        feed(&mut t, SNAPSHOT);
        t.apply_line(r#"{"ActivityRenamed":{"id":999,"name":"ghost"}}"#);
        assert_eq!(t.context().activity.as_deref(), Some("niri"));
    }

    #[test]
    fn workspaces_changed_no_focused_yields_none_workspace() {
        let mut t = Tracker::default();
        feed(&mut t, SNAPSHOT);
        t.apply_line(
            r#"{"WorkspacesChanged":{"workspaces":[
                {"id":14,"idx":2,"name":"alpha","output":"DP-3","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":null,"activities":[],"is_sticky":false,"is_in_active_activity":false}
            ]}}"#,
        );
        assert_eq!(t.context().workspace, None);
    }
}
