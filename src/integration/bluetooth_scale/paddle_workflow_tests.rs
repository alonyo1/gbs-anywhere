// Included inside run::tests so these exercise the same fake scale and Link.
struct FakeMini(tokio::sync::mpsc::Receiver<anyhow::Result<Event>>);
impl Events for FakeMini {
    async fn next(&mut self) -> anyhow::Result<Event> {
        self.0.recv().await.context("fake Mini disconnected")?
    }
}

struct Rig {
    link: Link,
    weights: tokio::sync::mpsc::Sender<Reading>,
    mini: tokio::sync::mpsc::Sender<anyhow::Result<Event>>,
    tares: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}
impl Drop for Rig { fn drop(&mut self) { self.task.abort(); } }
impl Rig {
    async fn new(existing: bool) -> Self {
        let link = link();
        if existing { start(&link, 0); }
        let (weights, rx) = tokio::sync::mpsc::channel(32);
        let (mini, events) = tokio::sync::mpsc::channel(16);
        let tares = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut scale = FakeScale { readings: rx, tares: tares.clone() };
        let l = link.clone();
        let task = tokio::spawn(async move {
            watch_with_mini(Model::Bookoo, &l, &mut scale, &mut l.grinder_brews(), &mut None, &mut FakeMini(events), true).await
        });
        weights.send(Reading { ms: 0, grams: 150.0 }).await.unwrap();
        crate::server::wait_for("paddle scale ready", || link.status.snapshot().health == Health::Connected).await;
        Self { link, weights, mini, tares, task }
    }
    fn tares(&self) -> usize { self.tares.load(std::sync::atomic::Ordering::SeqCst) }
    async fn start(&self) -> Instant {
        let at = Instant::now();
        self.mini.send(Ok(Event::Started { at })).await.unwrap();
        at
    }
}

#[tokio::test]
async fn paddle_tares_only_after_a_new_armed_grind_or_selected_test() {
    for test in [false, true] {
        let rig = Rig::new(false).await;
        rig.start().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(rig.tares(), 0, "unarmed paddle must not tare");
        if test {
            rig.link.server.with(|m, now| {
                m.start_shot(now).unwrap();
                rig.link.status.request_scale_test(now);
            });
        } else { start(&rig.link, 0); }
        crate::server::wait_for("paddle armed", || rig.link.status.snapshot().health == Health::Watching).await;
        assert_eq!(rig.tares(), 0, "grinder request only arms; it does not tare");
        rig.start().await;
        crate::server::wait_for("paddle tare", || rig.tares() == 1).await;
        rig.weights.send(Reading { ms: 100, grams: 0.0 }).await.unwrap();
        crate::server::wait_for("paddle timer", || rig.link.status.snapshot().live.is_some_and(|v| v.phase == ScalePhase::Brewing)).await;
        assert_eq!(rig.link.status.snapshot().live.unwrap().test, test);
        rig.mini.send(Err(anyhow::anyhow!("disconnected"))).await.unwrap();
        crate::server::wait_for("disconnect ends capture", || rig.task.is_finished()).await;
        assert_eq!(rig.link.status.snapshot().reports, 0);
        assert!(rig.link.server.with(|m, _| m.last_shot()).is_none());
    }
}

#[tokio::test]
async fn late_starts_reconnection_and_flushes_never_report() {
    let rig = Rig::new(true).await;
    rig.start().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(rig.tares(), 0, "reconnection cannot measure a waiting shot");
    drop(rig);
    let rig = Rig::new(false).await;
    start(&rig.link, 0);
    rig.mini.send(Ok(Event::Started { at: Instant::now() - Duration::from_secs(5) })).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(rig.tares(), 0, "old start never tares");
    rig.start().await;
    crate::server::wait_for("fresh start", || rig.tares() == 1).await;
    rig.mini.send(Ok(Event::Cancelled)).await.unwrap();
    crate::server::wait_for("flush discarded", || rig.task.is_finished()).await;
    assert!(rig.link.server.with(|m, _| m.last_shot()).is_none());
}

#[tokio::test]
async fn paddle_workflow_reports_duration_and_settled_weight_once() {
    let rig = Rig::new(false).await;
    start(&rig.link, 0);
    let at = rig.start().await;
    crate::server::wait_for("tare", || rig.tares() == 1).await;
    let mut stopped = false;
    for _ in 0..110 {
        let elapsed = at.elapsed();
        let ms = elapsed.as_millis() as u64 + 1;
        if elapsed >= Duration::from_secs(6) && !stopped {
            rig.mini.send(Ok(Event::Stopped { at: Instant::now(), time: Some(Duration::from_secs(6)) })).await.unwrap();
            stopped = true;
        }
        let grams = if ms < 1000 { 0.0 } else if ms < 6000 { (ms - 1000) as f64 * 0.0072 } else { 36.0 };
        rig.weights.send(Reading { ms, grams }).await.unwrap();
        if rig.link.status.snapshot().reports == 1 { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let shot = rig.link.server.with(|m, _| m.last_shot()).expect("finished paddle shot");
    assert_eq!(shot.time_ms, 6000);
    assert_eq!(shot.weight_g, 36.0);
    assert_eq!(rig.link.status.snapshot().reports, 1);
    assert_eq!(rig.tares(), 1);
}
