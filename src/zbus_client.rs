//! Real hamster client over zbus, plus best-effort desktop notifications.

use anyhow::Context as _;
use zbus::Connection;

use crate::hamster::{Fact, HamsterClient, NewFact};

#[zbus::proxy(
    interface = "org.gnome.Hamster",
    default_service = "org.gnome.Hamster",
    default_path = "/org/gnome/Hamster"
)]
pub trait Hamster {
    #[zbus(name = "GetTodaysFactsJSON")]
    fn get_todays_facts_json(&self) -> zbus::Result<Vec<String>>;
    #[zbus(name = "AddFactJSON")]
    fn add_fact_json(&self, fact: &str) -> zbus::Result<i32>;
    fn stop_tracking(&self, end_time: i32) -> zbus::Result<()>;
    #[zbus(signal)]
    fn facts_changed(&self) -> zbus::Result<()>;
}

pub struct ZbusHamster {
    proxy: HamsterProxy<'static>,
}

impl ZbusHamster {
    pub async fn connect() -> anyhow::Result<(Self, Connection)> {
        let conn = Connection::session().await.context("session bus")?;
        let proxy = HamsterProxy::new(&conn).await.context("hamster proxy")?;
        Ok((Self { proxy }, conn))
    }

    /// Raw proxy; use to subscribe to [`HamsterProxy::receive_facts_changed`].
    /// `'static` is sound: the proxy ref-counts the connection internally.
    pub fn proxy(&self) -> &HamsterProxy<'static> {
        &self.proxy
    }
}

impl HamsterClient for ZbusHamster {
    async fn todays_facts(&self) -> anyhow::Result<Vec<Fact>> {
        let raw = self
            .proxy
            .get_todays_facts_json()
            .await
            .context("GetTodaysFactsJSON")?;
        raw.iter()
            .map(|s| serde_json::from_str(s).context("parse fact"))
            .collect()
    }

    async fn add_fact(&self, fact: &NewFact) -> anyhow::Result<()> {
        let payload = serde_json::to_string(fact).context("serialize NewFact")?;
        let id = self
            .proxy
            .add_fact_json(&payload)
            .await
            .context("AddFactJSON")?;
        if id == 0 {
            // hamster's __add_fact returns 0 for "same fact already
            // on-going, nothing to do" — a benign no-op, not a rejection
            log::info!("hamster: fact already on-going, nothing to do");
        }
        Ok(())
    }

    async fn stop_tracking(&self) -> anyhow::Result<()> {
        // 0 = "server-side now" (non-zero hits a naive-UTC quirk upstream)
        self.proxy.stop_tracking(0).await.context("StopTracking")?;
        Ok(())
    }

    async fn notify(&self, message: &str) {
        // best-effort, never load-bearing
        let bin = std::env::var("NOTIFY_SEND_BIN").unwrap_or_else(|_| "notify-send".into());
        let res = tokio::process::Command::new(bin)
            .arg("jiji-hamster-bridge")
            .arg(message)
            .status()
            .await;
        match res {
            Err(e) => log::warn!("notify-send: spawn failed: {e}"),
            Ok(status) if !status.success() => {
                // expected on sessions without a notification daemon
                log::debug!("notify-send exited {status}");
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hamster::running_fact;

    /// Smoke test against the real session bus + live hamster-service.
    /// Read-only. Run manually: cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn live_todays_facts_parse() {
        let (client, _conn) = ZbusHamster::connect().await.unwrap();
        let facts = client.todays_facts().await.unwrap();
        // shape parsed; running fact detection works (may be None)
        let _ = running_fact(&facts);
    }
}
