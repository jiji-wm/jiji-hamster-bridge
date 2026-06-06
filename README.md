# jiji-hamster-bridge

Bridges [jiji](https://github.com/gajdusek/jiji) (a Wayland compositor) activity and workspace focus events to [GNOME Hamster](https://projecthamster.org/) time tracking over D-Bus. As you switch activities or named workspaces, the bridge pauses, resumes, and switches the running Hamster fact so it always matches the active desktop context. Hysteresis debouncing prevents timesheet fragmentation from transient window switches — starts are immediate, but stops and cross-context switches only commit after configurable grace periods. On resume, the bridge matches existing facts by `entity:` tag (today only) and creates tagged placeholder facts when nothing matches, so the Hamster timeline stays coherent without manual intervention.

## Configuration

`~/.config/jiji-hamster-bridge/config.toml` is hot-reloaded on change — invalid configs are rejected with a desktop notification and the previous config stays active. `SIGHUP` forces a reload.

```toml
[defaults]
switch_immediate = true          # tracked→tracked switches act instantly
return_debounce_secs = 60        # flap-back to the previous entity
untracked_grace_secs = 60        # leaving tracked context: grace before stop
entity_tag_key = "entity"        # tag key: "entity: work1"
extra_tags = ["location: home"]  # added to new placeholder facts
placeholder_activity = "placeholder"
placeholder_description = "auto-started by jiji-hamster-bridge — rename me"

[activities.work1]
entity = "work1"                 # default: the activity name itself
category = "work1.example"       # hamster category for placeholder facts

[activities.work2]
category = "work2.example"

[workspaces.invoicing]           # named workspace — overrides its activity
entity = "work1"

[workspaces.scratch]
track = false                    # untracked even inside a tracked activity
```

## Install

Preferred — from the jiji workspace root (installs the binary to
`~/.cargo/bin` **and** the systemd user unit, with daemon-reload and a
try-restart of an already-running daemon on upgrades):

```sh
scripts/install.sh jiji-hamster-bridge
```

Then enable once:

```sh
systemctl --user enable --now jiji-hamster-bridge
```

Bare-cargo alternative (binary only — copy the unit yourself):

```sh
cargo install --path . --offline
cp systemd/jiji-hamster-bridge.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now jiji-hamster-bridge
```

## Environment variables

| Variable          | Default        | Description                                      |
|-------------------|----------------|--------------------------------------------------|
| `JIJI_MSG_BIN`    | `jiji`         | Binary used for `jiji msg --json event-stream`   |
| `NOTIFY_SEND_BIN` | `notify-send`  | Binary used for desktop notifications            |
| `RUST_LOG`        | `info`         | Log level (tracing env filter)                   |

CLI flag: `--config <path>` overrides the default config location.

## Requirements

- **hamster-time-tracker** — provides the `org.gnome.Hamster` D-Bus service.
- A running **jiji** compositor session (the `$JIJI_SOCKET` env var must be set, or `JIJI_MSG_BIN` must resolve to a working binary).
