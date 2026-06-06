use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::Sender;

use jiji_hamster_bridge::config::Config;
use jiji_hamster_bridge::run::{LoopInput, run_loop};
use jiji_hamster_bridge::zbus_client::ZbusHamster;

fn config_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(2)
        && std::env::args().nth(1).as_deref() == Some("--config")
    {
        return PathBuf::from(arg);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").expect("HOME")).join(".config"));
    base.join("jiji-hamster-bridge/config.toml")
}

fn load_config(path: &std::path::Path) -> anyhow::Result<Config> {
    let s = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Config::parse(&s)
}

/// Feed event-stream lines, reconnecting with backoff on subprocess death.
async fn event_stream_task(tx: Sender<LoopInput>) {
    let bin = std::env::var("JIJI_MSG_BIN").unwrap_or_else(|_| "jiji".into());
    let mut backoff = Duration::from_secs(1);
    loop {
        let child = tokio::process::Command::new(&bin)
            .args(["msg", "--json", "event-stream"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        match child {
            Ok(mut child) => {
                let stdout = child.stdout.take().expect("piped stdout");
                let mut lines = BufReader::new(stdout).lines();
                let mut got_any = false;
                while let Ok(Some(line)) = lines.next_line().await {
                    got_any = true;
                    backoff = Duration::from_secs(1);
                    if tx.send(LoopInput::EventLine(line)).await.is_err() {
                        return;
                    }
                }
                let _ = child.wait().await;
                if got_any {
                    log::warn!("event stream ended; reconnecting");
                }
            }
            Err(e) => log::error!("spawn {bin}: {e}"),
        }
        if tx.send(LoopInput::StreamReset).await.is_err() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

/// Watch the config file + SIGHUP; send validated configs, notify on errors.
async fn config_reload_task(tx: Sender<LoopInput>, path: PathBuf) -> anyhow::Result<()> {
    use notify::Watcher as _;
    let (raw_tx, mut raw_rx) = tokio::sync::mpsc::channel::<()>(4);
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = raw_tx.try_send(());
        }
    })
    .context("create watcher")?;
    // watch the parent dir: editors and chezmoi replace the file atomically
    let dir = path
        .parent()
        .expect("config has a parent dir")
        .to_path_buf();
    if let Err(e) = watcher.watch(&dir, notify::RecursiveMode::NonRecursive) {
        log::warn!("config watch failed ({e}); SIGHUP still works");
    }
    let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .context("install SIGHUP handler")?;
    loop {
        tokio::select! {
            r = raw_rx.recv() => if r.is_none() { return Ok(()) },
            _ = hup.recv() => {}
        }
        // debounce bursts of fs events
        tokio::time::sleep(Duration::from_millis(200)).await;
        while raw_rx.try_recv().is_ok() {}
        match load_config(&path) {
            Ok(cfg) => {
                log::info!("config reloaded");
                if tx.send(LoopInput::ConfigReloaded(cfg)).await.is_err() {
                    return Ok(());
                }
            }
            Err(e) => {
                log::error!("config reload failed, keeping previous: {e:#}");
                let bin = std::env::var("NOTIFY_SEND_BIN").unwrap_or_else(|_| "notify-send".into());
                let _ = tokio::process::Command::new(bin)
                    .arg("jiji-hamster-bridge")
                    .arg(format!("config invalid, still using previous — {e}"))
                    .status()
                    .await;
            }
        }
    }
}

/// Subscribe to hamster's FactsChanged signal; each firing triggers a reconcile.
async fn facts_changed_task(
    tx: Sender<LoopInput>,
    proxy: jiji_hamster_bridge::zbus_client::HamsterProxy<'static>,
) {
    use futures_util::StreamExt as _;
    let mut stream = match proxy.receive_facts_changed().await {
        Ok(s) => s,
        Err(e) => {
            log::error!("cannot subscribe to FactsChanged: {e:#}; reconcile disabled");
            return;
        }
    };
    while stream.next().await.is_some() {
        if tx.send(LoopInput::FactsChanged).await.is_err() {
            return;
        }
    }
    // zbus follows hamster restarts via NameOwnerChanged; the stream only
    // ends when the bus connection itself dies
    log::warn!("FactsChanged stream ended; reconcile disabled for this session");
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let path = config_path();
    let cfg = load_config(&path)?; // startup: invalid config is fatal (nothing to keep)

    let (client, conn) = ZbusHamster::connect().await?;
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(facts_changed_task(tx.clone(), client.proxy().clone()));
    tokio::spawn(event_stream_task(tx.clone()));
    {
        let tx = tx.clone();
        let path = path.clone();
        tokio::spawn(async move {
            if let Err(e) = config_reload_task(tx, path).await {
                log::error!("config reload task failed: {e:#}; reload + SIGHUP disabled");
            }
        });
    }
    drop(tx);

    run_loop(rx, client, cfg).await;
    drop(conn);
    Ok(())
}
