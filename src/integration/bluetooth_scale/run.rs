//! Connect locally, then measure grinder requests or explicitly selected tests.

use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use super::{
    Line, Model,
    config::Config,
    mini::{Event, Events, NoMini},
    paddle::Capture,
};
use crate::integration::scale_shot::{Detector, Progress, Reading, Shot};
use crate::integration::{
    Backoff, BoxFuture, Brew, Detail, GrinderBrews, Integration, Link, ScalePhase,
};

const SILENCE: Duration = Duration::from_secs(3);
const MAX_GAP_MS: u64 = 2000;

pub(crate) trait Scale: Send {
    fn next(&mut self) -> impl Future<Output = anyhow::Result<Reading>> + Send;
    fn tare(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send;
}

pub(crate) struct BluetoothScale {
    pub(crate) cfg: Config,
    pub(crate) model: Model,
}

impl Integration for BluetoothScale {
    fn run(self: Box<Self>, link: Link) -> BoxFuture {
        Box::pin(async move {
            #[cfg(target_os = "linux")]
            run(link, self.cfg, self.model).await;
            #[cfg(not(target_os = "linux"))]
            link.status.error(format!(
                "Bluetooth scales require Linux and BlueZ ({} on {})",
                self.cfg.address, self.cfg.adapter
            ));
        })
    }
}

fn idle(model: Model, last: Option<Shot>, grams: f64) -> Detail {
    model.detail(match last {
        None => Line::Idle(grams),
        Some(s) => Line::LastShot(s),
    })
}

fn progress(model: Model, p: &Progress, test: bool) -> Detail {
    if test {
        return model.detail(Line::Testing);
    }
    model.detail(match *p {
        Progress::Settling => Line::Settling,
        Progress::Ready => Line::Ready,
        Progress::Flowing { grams } => Line::Flowing(grams),
        Progress::Done(s) => Line::LastShot(s),
    })
}

#[cfg(target_os = "linux")]
async fn run(link: Link, cfg: Config, model: Model) {
    let mut addresses = vec![cfg.address.as_str()];
    if let Some(mini) = &cfg.mini {
        addresses.push(&mini.address);
    }
    let _claim = super::wait_claim(&link, &cfg.adapter, &addresses).await;

    link.status
        .subject(format!("{} · {}", model.title(), cfg.adapter));
    let mut brews = link.grinder_brews();
    let mut backoff = Backoff::new(Duration::from_secs(2), Duration::from_secs(10));
    let mut last = None;
    loop {
        link.status.starting(model.detail(Line::Connecting));
        let result = async {
            // Establish the Mini first: a scale must not become ready while
            // its selected timing source is still connecting.
            let mini = match &cfg.mini {
                Some(c) => Some(super::mini::bluez::connect(c, &cfg.adapter).await?),
                None => None,
            };
            let mut scale = super::bluez::connect(&cfg, model).await?;
            backoff.reset();
            let result = match mini {
                Some(mut mini) => {
                    watch_with_mini(
                        model, &link, &mut scale, &mut brews, &mut last, &mut mini, true,
                    )
                    .await
                }
                None => watch(model, &link, &mut scale, &mut brews, &mut last).await,
            };
            scale.close().await;
            result
        }
        .await;
        if matches!(result, Ok(())) {
            return;
        }
        if let Err(e) = result {
            tracing::warn!("{}: {e:#}", model.title());
            link.status.error(format!("{e:#}"));
        }
        // A reconnect starts with no active detector. An interrupted shot
        // cannot be reconstructed from only its remaining weight readings.
        backoff.retry(model.title()).await;
    }
}

struct Measurement {
    model: Model,
    active: Option<(Brew, Detector)>,
    previous_ms: Option<u64>,
    tare_since: Option<Option<u64>>,
    test: bool,
    phase: ScalePhase,
    time_ms: Option<u64>,
    paddle: Option<Capture>,
}

impl Measurement {
    fn new(model: Model, first: Reading) -> Self {
        Self {
            model,
            active: None,
            previous_ms: Some(first.ms),
            tare_since: None,
            test: false,
            phase: ScalePhase::Idle,
            time_ms: None,
            paddle: None,
        }
    }

    fn arm(&mut self, brew: Brew, link: &Link) {
        self.arm_for(brew, link, false);
    }

    fn arm_for(&mut self, brew: Brew, link: &Link, test: bool) {
        self.paddle = None;
        self.tare_since = Some(None);
        self.test = test;
        self.phase = ScalePhase::Waiting;
        self.time_ms = Some(0);
        self.active = Some((
            brew,
            Detector::after_tare().expecting(if test {
                None
            } else {
                link.server.with(|m, _| m.recipe_weight_g())
            }),
        ));
    }

    fn still_waiting(&self, link: &Link) -> bool {
        self.active.as_ref().is_some_and(|(brew, _)| {
            if self.test {
                link.test_waits_for(*brew)
            } else {
                link.waits_for(*brew)
            }
        })
    }

    fn read(&mut self, r: Reading, link: &Link) -> anyhow::Result<Option<Shot>> {
        if let Some(prev) = self.previous_ms {
            if r.ms < prev || r.ms.saturating_sub(prev) > MAX_GAP_MS {
                self.active = None;
                bail!("Bluetooth readings were interrupted; enter this shot manually");
            }
            if r.ms == prev {
                return Ok(None);
            }
        }
        self.previous_ms = Some(r.ms);
        if !r.grams.is_finite() {
            bail!("invalid scale weight");
        }
        let Some((brew, detector)) = &mut self.active else {
            return Ok(None);
        };
        if !(if self.test {
            link.test_waits_for(*brew)
        } else {
            link.waits_for(*brew)
        }) {
            self.active = None;
            return Ok(None);
        }
        if let Some(paddle) = &mut self.paddle {
            let now = Instant::now();
            let shot = paddle.read(r, now)?;
            self.phase = if paddle.stopped() {
                ScalePhase::Draining
            } else {
                ScalePhase::Brewing
            };
            self.time_ms = Some(paddle.time_ms(now));
            link.status.watching(self.model.detail(if paddle.stopped() {
                Line::Draining
            } else {
                Line::Brewing
            }));
            if let Some(shot) = shot {
                if self.test {
                    link.finish_test(*brew, shot.time, shot.grams);
                } else {
                    link.report(
                        *brew,
                        shot.time,
                        Some((
                            shot.grams,
                            "weighed by the Bluetooth scale; timed by the Mini paddle".into(),
                        )),
                    );
                }
                self.active = None;
                return Ok(Some(shot));
            }
            return Ok(None);
        }
        // A successful Bluetooth write does not mean the scale accepted
        // tare in its current mode. Never establish a baseline from a cup
        // already filling when a tare command was ignored.
        if let Some(since) = &mut self.tare_since {
            let since = *since.get_or_insert(r.ms);
            if r.grams.abs() <= 0.5 {
                self.tare_since = None;
            } else if r.ms.saturating_sub(since) > 1500 {
                self.active = None;
                bail!("scale did not tare; use manual weighing mode and enter this shot manually");
            } else {
                return Ok(None);
            }
        }
        let state = detector.push(r);
        self.phase = match state {
            Progress::Flowing { .. } => ScalePhase::Flowing,
            Progress::Done(_) => ScalePhase::Done,
            _ => ScalePhase::Waiting,
        };
        self.time_ms = detector.time_ms().or(Some(0));
        link.status
            .watching(progress(self.model, &state, self.test));
        if let Progress::Done(shot) = state {
            if self.test {
                link.finish_test(*brew, shot.time, shot.grams);
            } else {
                link.report(
                    *brew,
                    shot.time,
                    Some((shot.grams, "weighed by the Bluetooth scale".into())),
                );
            }
            self.active = None;
            return Ok(Some(shot));
        }
        Ok(None)
    }
}

/// Requires fresh readings before accepting a grind. A shot already waiting
/// at connection time stays for manual reporting, without a late tare.
async fn watch(
    model: Model,
    link: &Link,
    scale: &mut impl Scale,
    brews: &mut GrinderBrews,
    last: &mut Option<Shot>,
) -> anyhow::Result<()> {
    watch_with_mini(model, link, scale, brews, last, &mut NoMini, false).await
}

async fn watch_with_mini(
    model: Model,
    link: &Link,
    scale: &mut impl Scale,
    brews: &mut GrinderBrews,
    last: &mut Option<Shot>,
    mini: &mut impl Events,
    paddle_timing: bool,
) -> anyhow::Result<()> {
    let mut tests = link.status.scale_tests();
    tests.borrow_and_update();
    let first = tokio::time::timeout(SILENCE, scale.next())
        .await
        .context("the scale connected but sent no valid weights")??;
    let blocked = link.waiting_brew();
    let mut pending: Option<(Brew, bool)> = None;
    let mut current_grams = first.grams;
    let mut measurement = Measurement::new(model, first);
    let mut last_received = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    link.status.connected(idle(model, *last, current_grams));
    link.status.scale_live(
        current_grams,
        last.map(|s| s.time.as_millis() as u64),
        ScalePhase::Idle,
        None,
        false,
        paddle_timing,
    );
    loop {
        if measurement.active.is_some() && !measurement.still_waiting(link) {
            measurement.active = None;
            measurement.paddle = None;
        }
        let mut received = false;
        tokio::select! {
            biased;
            changed = tests.changed() => {
                if changed.is_err() { return Ok(()); }
                let requested = *tests.borrow_and_update();
                if let Some(brew) = requested.filter(|b| link.test_waits_for(*b)) {
                    if last_received.elapsed() > Duration::from_secs(1) {
                        bail!("scale was not ready for the test; wait for it to reconnect");
                    }
                    if paddle_timing {
                        pending = Some((brew, true));
                        link.status.watching(model.detail(Line::Paddle));
                    } else {
                        scale.tare().await.context("could not tare the scale")?;
                        measurement.arm_for(brew, link, true);
                        link.status.watching(progress(model, &Progress::Settling, true));
                    }
                }
            }
            next = brews.next() => {
                let Some(brew) = next else { return Ok(()) };
                if Some(brew) == blocked || measurement.active.as_ref().is_some_and(|(b, _)| *b == brew) { continue; }
                if last_received.elapsed() > Duration::from_secs(1) {
                    bail!("scale was not ready at the grind; enter this shot manually");
                }
                if paddle_timing {
                    pending = Some((brew, false));
                    measurement.active = None;
                    measurement.paddle = None;
                    link.status.watching(model.detail(Line::Paddle));
                } else {
                    scale.tare().await.context("could not tare the scale")?;
                    measurement.arm(brew, link);
                    link.status.watching(progress(model, &Progress::Settling, false));
                }
            }
            event = mini.next(), if paddle_timing => {
                match event? {
                    Event::Started { at } => {
                        // The grinder event and the BLE start can arrive together.
                        // Consult the current brew as well as the request stream.
                        let requested = pending.or_else(|| link.waiting_brew()
                            .filter(|b| Some(*b) != blocked).map(|b| (b, false)));
                        if measurement.active.is_some() { bail!("Mini restarted before the previous cup settled; enter this shot manually"); }
                        if let Some((brew, test)) = requested.filter(|(b, t)| if *t { link.test_waits_for(*b) } else { link.waits_for(*b) }) {
                            if at < brew.started_at() { continue; }
                            if at.elapsed() > Duration::from_millis(500) || last_received.elapsed() > Duration::from_secs(1) {
                                bail!("Mini start or scale readings arrived too late to tare; enter this shot manually");
                            }
                            pending = None;
                            scale.tare().await.context("could not tare the scale at paddle start")?;
                            if at.elapsed() > Duration::from_millis(1000) { bail!("tare command was too slow; enter this shot manually"); }
                            measurement.arm_for(brew, link, test);
                            measurement.paddle = Some(Capture::new(at, if test { None } else { link.server.with(|m, _| m.recipe_weight_g()) }));
                            measurement.phase = ScalePhase::Brewing;
                            link.status.watching(model.detail(Line::Brewing));
                        }
                    }
                    Event::Stopped { at, time } => {
                        if measurement.still_waiting(link) && let Some(p) = &mut measurement.paddle { p.stop(at, time)?; }
                    }
                    Event::Cancelled => {
                        if measurement.still_waiting(link) { bail!("Mini classified this as a flush or cleaning cycle; enter this shot manually"); }
                    }
                }
            }
            reading = scale.next() => {
                let reading = reading.context("lost the Bluetooth scale; enter this shot manually")?;
                if last_received.elapsed() > Duration::from_millis(MAX_GAP_MS) {
                    bail!("Bluetooth readings were interrupted; enter this shot manually");
                }
                last_received = Instant::now();
                received = true;
                current_grams = reading.grams;
                if let Some(shot) = measurement.read(reading, link)? {
                    *last = Some(shot);
                }
            }
            _ = tick.tick() => {
                if last_received.elapsed() > SILENCE {
                    bail!("the scale stopped sending valid weights; turn it on and close other scale apps");
                }
            }
        }
        if measurement.active.is_some() && !measurement.still_waiting(link) {
            measurement.active = None;
        }
        if pending.is_some_and(|(b, test)| {
            !(if test {
                link.test_waits_for(b)
            } else {
                link.waits_for(b)
            })
        }) {
            pending = None;
        }
        if measurement.active.is_none() && pending.is_none() {
            if blocked.is_some() && link.waiting_brew() == blocked {
                link.status.connected(model.detail(Line::Late));
            } else {
                link.status.connected(idle(model, *last, current_grams));
            }
        }
        // Only new readings refresh the freshness clock. A timer tick or
        // a stalled radio must not make old weight appear current.
        if received {
            if let Some((brew, _)) = &measurement.active {
                link.status.scale_live(
                    current_grams,
                    measurement.time_ms,
                    measurement.phase,
                    Some(*brew),
                    measurement.test,
                    paddle_timing,
                );
            } else if let Some((brew, test)) = pending {
                link.status.scale_live(
                    current_grams,
                    Some(0),
                    ScalePhase::Paddle,
                    Some(brew),
                    test,
                    paddle_timing,
                );
            } else {
                link.status.scale_live(
                    current_grams,
                    last.map(|s| s.time.as_millis() as u64),
                    ScalePhase::Idle,
                    None,
                    false,
                    paddle_timing,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::bookoo_bluetooth::TITLE;
    use crate::integration::scale_shot::synth::{espresso, readings};
    use crate::integration::{Health, Status};
    use crate::protocol::MachineConfig;
    use crate::server::Server;

    fn link() -> Link {
        Link {
            server: Server::new(MachineConfig::default()),
            status: Status::new("bookoo_bluetooth", "bookoo_bluetooth", TITLE),
        }
    }

    fn start(link: &Link, seconds: u64) -> Brew {
        link.server.with(|m, _| {
            m.on_start_request(Instant::now() + Duration::from_secs(seconds), Some(9))
        });
        link.waiting_brew().unwrap()
    }

    #[test]
    fn measured_shot_is_reported_only_for_its_grinder_request() {
        let l = link();
        let mut m = Measurement::new(Model::Bookoo, Reading { ms: 0, grams: 0.0 });
        let brew = start(&l, 30);
        m.arm(brew, &l);
        let mut result = None;
        for r in readings(100, 30.0, |t| espresso(t, 18.0, 2.0)) {
            result = result.or(m.read(r, &l).unwrap());
        }
        let shot = result.expect("a complete shot");
        assert!((shot.grams - 36.3).abs() < 0.5);
        assert!((shot.time.as_secs_f64() - 18.0).abs() < 2.0);
        assert!(!l.waits_for(brew));
    }

    #[test]
    fn idle_scale_and_replaced_brew_never_report() {
        let l = link();
        let mut m = Measurement::new(Model::Bookoo, Reading { ms: 0, grams: 0.0 });
        for r in readings(100, 30.0, |t| espresso(t, 18.0, 2.0)) {
            assert!(m.read(r, &l).unwrap().is_none());
        }
        let brew = start(&l, 30);
        m.arm(brew, &l);
        // A new grind replaces the old one; its weight cannot be reused.
        l.server.with(|machine, now| machine.abort(now)).unwrap();
        let newer = start(&l, 60);
        assert_ne!(newer, brew);
        for r in readings(30100, 30.0, |t| espresso(t, 18.0, 2.0)) {
            assert!(m.read(r, &l).unwrap().is_none());
        }
        assert!(l.waits_for(newer));
        assert_eq!(l.status.snapshot().reports, 0);
    }

    #[test]
    fn a_gap_or_clock_reset_discards_partial_shots() {
        for ms in [0, 5000] {
            let l = link();
            let mut m = Measurement::new(
                Model::Bookoo,
                Reading {
                    ms: 100,
                    grams: 0.0,
                },
            );
            let brew = start(&l, 30);
            m.arm(brew, &l);
            assert!(m.read(Reading { ms, grams: 36.0 }, &l).is_err());
            assert!(m.active.is_none());
            assert!(l.waits_for(brew));
        }
    }

    #[test]
    fn ignored_tare_never_establishes_a_partial_shot_baseline() {
        let l = link();
        let mut m = Measurement::new(
            Model::Bookoo,
            Reading {
                ms: 0,
                grams: 150.0,
            },
        );
        let brew = start(&l, 30);
        m.arm(brew, &l);
        let mut rejected = false;
        for r in readings(100, 3.0, |_| 150.0) {
            if m.read(r, &l).is_err() {
                rejected = true;
                break;
            }
        }
        assert!(rejected);
        assert!(m.active.is_none());
        assert!(l.waits_for(brew));
        assert_eq!(l.status.snapshot().reports, 0);
    }

    struct FakeScale {
        readings: tokio::sync::mpsc::Receiver<Reading>,
        tares: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Scale for FakeScale {
        async fn next(&mut self) -> anyhow::Result<Reading> {
            self.readings
                .recv()
                .await
                .context("fake scale disconnected")
        }
        async fn tare(&mut self) -> anyhow::Result<()> {
            self.tares.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn ready_scale_tares_once_and_reports_a_complete_shot() {
        exercise_shot(false).await;
    }

    #[tokio::test]
    async fn selected_scale_test_measures_without_a_grinder_report() {
        exercise_shot(true).await;
    }

    async fn exercise_shot(test: bool) {
        let l = link();
        let (tx, rx) = tokio::sync::mpsc::channel(512);
        let tares = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut fake = FakeScale {
            readings: rx,
            tares: tares.clone(),
        };
        let task_link = l.clone();
        let task = tokio::spawn(async move {
            watch(
                Model::Bookoo,
                &task_link,
                &mut fake,
                &mut task_link.grinder_brews(),
                &mut None,
            )
            .await
        });
        tx.send(Reading {
            ms: 0,
            grams: 150.0,
        })
        .await
        .unwrap();
        crate::server::wait_for("scale ready", || {
            l.status.snapshot().health == Health::Connected
        })
        .await;
        let brew = if test {
            l.server.with(|m, now| {
                m.start_shot(now).unwrap();
                l.status.request_scale_test(now);
            });
            l.status.snapshot().test_brew.unwrap()
        } else {
            start(&l, 30)
        };
        crate::server::wait_for("tare", || {
            tares.load(std::sync::atomic::Ordering::SeqCst) == 1
        })
        .await;
        for r in readings(100, 30.0, |t| espresso(t, 18.0, 2.0)) {
            // Feed the documented wire representation through the parser.
            let packet = crate::integration::bookoo_bluetooth::protocol::frame(r.grams);
            let grams = crate::integration::bookoo_bluetooth::protocol::grams(&packet).unwrap();
            tx.send(Reading { ms: r.ms, grams }).await.unwrap();
        }
        crate::server::wait_for("reported shot", || {
            l.server.with(|m, _| m.last_shot().is_some())
        })
        .await;
        assert!(!l.waits_for(brew));
        assert!(!l.test_waits_for(brew));
        assert_eq!(tares.load(std::sync::atomic::Ordering::SeqCst), 1);
        let reported = l.server.with(|m, _| m.last_shot()).unwrap();
        assert!((reported.weight_g - 36.3).abs() < 0.5);
        drop(tx);
        assert!(task.await.unwrap().is_err());
        assert_eq!(l.status.snapshot().reports, if test { 0 } else { 1 });
        assert_eq!(l.status.snapshot().last_report.is_some(), !test);
    }

    #[tokio::test]
    async fn connecting_during_a_shot_does_not_tare_or_report_it() {
        let l = link();
        let brew = start(&l, 30);
        let (tx, rx) = tokio::sync::mpsc::channel(512);
        let tares = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fake = FakeScale {
            readings: rx,
            tares: tares.clone(),
        };
        let task_link = l.clone();
        let task = tokio::spawn(async move {
            watch(
                Model::Bookoo,
                &task_link,
                &mut { fake },
                &mut task_link.grinder_brews(),
                &mut None,
            )
            .await
        });
        for r in readings(100, 30.0, |t| espresso(t, 18.0, 2.0)) {
            tx.send(r).await.unwrap();
        }
        drop(tx);
        assert!(task.await.unwrap().is_err());
        assert_eq!(tares.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(l.waits_for(brew));
        assert_eq!(l.status.snapshot().health, Health::Connected);
    }
    include!("paddle_workflow_tests.rs");
}
