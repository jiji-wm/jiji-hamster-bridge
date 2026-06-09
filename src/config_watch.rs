//! Decide whether a filesystem-watcher event warrants a config reload.
//!
//! Kept as a pure, I/O-free predicate so it is unit-testable without an
//! inotify backend, in line with this crate's pure-core / async-shell split.

use std::path::Path;

use notify::Event;
use notify::event::{EventKind, ModifyKind};

/// Whether `event` should trigger a config reload attempt for `config_file`.
///
/// The reload path opens and reads the config file, and those reads surface as
/// `Access(_)` events (open / read / close) plus atime-only
/// `Modify(Metadata(_))` events. Treating them as reload triggers makes the
/// watcher fire on its own reads — a self-sustaining loop that reloads several
/// times a second forever. We therefore accept only content-changing or
/// structural events (`Create`, `Remove`, data / rename `Modify`, and the
/// catch-all `Any` / `Other`) for the config file itself.
///
/// Matching is by file name: atomic replacement (chezmoi and most editors)
/// renames a temp file over the config, so the meaningful event's path is the
/// config file even though we watch its parent directory.
pub fn is_relevant_event(event: &Event, config_file: &Path) -> bool {
    let kind_relevant = match event.kind {
        // Reads of the config — what the reload itself performs.
        EventKind::Access(_) => false,
        // atime / permission churn, including the atime bump from our own read.
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        // Genuine content / structural changes.
        EventKind::Create(_)
        | EventKind::Remove(_)
        | EventKind::Modify(_)
        | EventKind::Any
        | EventKind::Other => true,
    };
    kind_relevant
        && event
            .paths
            .iter()
            .any(|p| p.file_name() == config_file.file_name())
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::Event;
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind,
        RenameMode,
    };
    use std::path::PathBuf;

    const CFG: &str = "/home/x/.config/jiji-hamster-bridge/config.toml";

    fn ev(kind: EventKind, path: &str) -> Event {
        Event::new(kind).add_path(PathBuf::from(path))
    }

    // The exact events the self-trigger loop was built from: our own reads.
    #[test]
    fn access_open_read_close_are_ignored() {
        for kind in [
            EventKind::Access(AccessKind::Open(AccessMode::Read)),
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            EventKind::Access(AccessKind::Any),
        ] {
            assert!(!is_relevant_event(&ev(kind, CFG), Path::new(CFG)));
        }
    }

    #[test]
    fn atime_metadata_bump_is_ignored() {
        let kind = EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime));
        assert!(!is_relevant_event(&ev(kind, CFG), Path::new(CFG)));
    }

    #[test]
    fn data_write_triggers_reload() {
        let kind = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        assert!(is_relevant_event(&ev(kind, CFG), Path::new(CFG)));
    }

    #[test]
    fn create_triggers_reload() {
        assert!(is_relevant_event(
            &ev(EventKind::Create(CreateKind::File), CFG),
            Path::new(CFG)
        ));
    }

    // chezmoi / editor atomic replace: temp file renamed over the config.
    #[test]
    fn rename_to_config_triggers_reload() {
        let kind = EventKind::Modify(ModifyKind::Name(RenameMode::To));
        assert!(is_relevant_event(&ev(kind, CFG), Path::new(CFG)));
    }

    #[test]
    fn remove_triggers_reload() {
        assert!(is_relevant_event(
            &ev(EventKind::Remove(RemoveKind::File), CFG),
            Path::new(CFG)
        ));
    }

    // A write to some other file in the watched directory must not reload.
    #[test]
    fn unrelated_file_in_dir_is_ignored() {
        let other = "/home/x/.config/jiji-hamster-bridge/notes.txt";
        let kind = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        assert!(!is_relevant_event(&ev(kind, other), Path::new(CFG)));
    }

    // Unknown backend events stay permissive — the content guard dedups them.
    #[test]
    fn any_kind_for_config_triggers_reload() {
        assert!(is_relevant_event(&ev(EventKind::Any, CFG), Path::new(CFG)));
    }

    // An event carrying no path can't be attributed to the config — ignore it.
    #[test]
    fn pathless_event_is_ignored() {
        let event = Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)));
        assert!(!is_relevant_event(&event, Path::new(CFG)));
    }
}
