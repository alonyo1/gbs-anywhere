//! One owner of the physical scale; native Mini commands drive tare/timer.
use super::config::Config;
use crate::integration::{BoxFuture, Integration, Link};
pub(super) struct Bridge {
    pub cfg: Config,
}
impl Integration for Bridge {
    fn run(self: Box<Self>, link: Link) -> BoxFuture {
        Box::pin(async move {
            #[cfg(target_os = "linux")]
            run(link, self.cfg).await;
            #[cfg(not(target_os = "linux"))]
            {
                let _ = self.cfg;
                link.status
                    .error("The brew-by-weight bridge requires Linux and BlueZ");
            }
        })
    }
}
#[cfg(target_os = "linux")]
async fn run(link: Link, cfg: Config) {
    use crate::integration::{Backoff, Detail};
    use std::time::Duration;
    let _claim = crate::integration::bluetooth_scale::wait_claim(
        &link,
        &cfg.adapter,
        &[
            &cfg.address,
            &cfg.mini.as_ref().unwrap().address,
            "@virtual-scale",
        ],
    )
    .await;
    link.status
        .subject(format!("BOOKOO → LUNAR-C · {}", cfg.adapter));
    let mut backoff = Backoff::new(Duration::from_secs(2), Duration::from_secs(30));
    loop {
        link.status.starting(Detail::keyed(
            "bookoo_brew_by_weight.connecting",
            "Connecting the BOOKOO brew-by-weight bridge",
        ));
        let mut retry = true;
        if let Err(e) = session(&link, &cfg, &mut backoff, &mut retry).await {
            link.status.error(format!("{e:#}"));
            tracing::warn!("BOOKOO bridge: {e:#}");
        }
        if !retry {
            link.status.error("Bridge stopped after an interrupted brew or scale command. Stop the Mini with its paddle, then save this integration's settings to reconnect.");
            std::future::pending::<()>().await;
        }
        backoff.retry(super::TITLE).await;
    }
}
#[cfg(target_os = "linux")]
async fn session(
    link: &Link,
    cfg: &Config,
    backoff: &mut crate::integration::Backoff,
    retry: &mut bool,
) -> anyhow::Result<()> {
    use super::{gatt::Peripheral, source::Source};
    use crate::integration::bluetooth_scale::{Model, bluez};
    // Register GATT before connecting the source. Advertise only after a fresh
    // measurement, while the independent reader continues receiving callbacks.
    let mut peripheral =
        Peripheral::open(&cfg.adapter, &cfg.mini.as_ref().unwrap().address).await?;
    let result = async {
        let mut source = Source::new(bluez::connect(cfg, Model::Bookoo).await?);
        source.latest().await?;
        peripheral.advertise().await?;
        let first = source.latest().await?;
        backoff.reset();
        watch(link, cfg, &mut source, &mut peripheral, first, retry).await
    }
    .await;
    peripheral.close().await;
    while let Ok(event) = peripheral.events.try_recv() {
        if matches!(event, super::gatt::Event::Command(..)) {
            // A queued start may have raced the failed source read. Do not
            // reconnect automatically with an unknown machine state.
            *retry = false;
        }
    }
    result
}
#[cfg(target_os = "linux")]
async fn watch(
    link: &Link,
    cfg: &Config,
    source: &mut super::source::Source,
    peripheral: &mut super::gatt::Peripheral,
    first: (super::wire::Sample, std::time::Instant),
    retry: &mut bool,
) -> anyhow::Result<()> {
    use super::{
        gatt::Event,
        recording::Recording,
        wire::{Core, FRESH},
    };
    use anyhow::Context;
    use std::time::{Duration, Instant};
    let mut core = Core::new(first.0, first.1);
    let mut brewing = false;
    let (available_tx, available_rx) = tokio::sync::watch::channel(true);
    let mut registration: Option<Task> = None;
    let mut retry_at = Instant::now();
    let mut failures = 0u32;
    let mut recording = Recording::new(link);
    let mut registration_error: Option<String> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            sample=source.next()=> {
                let (s,at)=sample?;core.reading(s,at)?;
                recording.reading(s,at,peripheral.subscribed() && core.paired,registration_error.as_deref(),link);
            }
            event=peripheral.events.recv()=>match event.context("virtual scale service ended")? {
                Event::Closed=>anyhow::bail!("Mini disconnected from the virtual scale; active GbS measurement discarded"),
                Event::Subscribed=>{available_tx.send_replace(false);},
                Event::Command(bytes,at)=> {
                    *retry=false;
                    anyhow::ensure!(at.elapsed()<=FRESH,"Mini scale command arrived late");
                    if let Err(e)=core.command(&bytes,at){*retry=false;return Err(e);}
                    while let Some((action,at))=core.event(){
                        if action==super::wire::Action::Start {brewing=true;}
                        if action==super::wire::Action::Stop {brewing=false;}
                        recording.event(action,at,link);
                    }
                    *retry=!brewing && !core.pending_controls();
                }
            },
            _=tick.tick()=>{},
        }
        *retry = !brewing && !core.pending_controls();
        let now = Instant::now();
        core.check(now)?;
        peripheral.check()?;
        if let Some(action) = core.next_write(now)? {
            source.writer.write(action.bytes()).await?;
        }
        if peripheral.subscribed()
            && let Some(bytes) = core.output(Instant::now())?
        {
            peripheral.notify(bytes).await?;
            core.delivered();
        }
        if registration.as_ref().is_some_and(|t| t.0.is_finished()) {
            let mut task = registration.take().unwrap();
            if let Err(e) = (&mut task.0)
                .await
                .context("Mini registration task stopped")?
            {
                registration_error = Some(e.to_string());
                tracing::warn!("BOOKOO bridge: {e}");
            }
        }
        if registration.is_none() && !peripheral.subscribed() && now >= retry_at {
            available_tx.send_replace(true);
            registration = Some(Task(tokio::spawn(super::registration::register(
                cfg.mini.clone().unwrap(),
                cfg.adapter.clone(),
                available_rx.clone(),
            ))));
            retry_at = now + Duration::from_secs(30 << failures.min(3));
            failures = failures.saturating_add(1);
        }
    }
}
#[cfg(target_os = "linux")]
struct Task(tokio::task::JoinHandle<anyhow::Result<()>>);
#[cfg(target_os = "linux")]
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
