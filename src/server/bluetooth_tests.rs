//! Scale tests go through the real HTTP routes without touching host Bluetooth.
use super::*;
use crate::integration::{BoxFuture, Integration, Kind, Link, ReportOutcome, ScalePhase, Settings};
use crate::protocol::Phase;

struct ReadyScale;
impl Integration for ReadyScale {
    fn run(self: Box<Self>, link: Link) -> BoxFuture {
        Box::pin(async move {
            let mut tests = link.status.scale_tests();
            link.status.connected("ready");
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                tokio::select! {
                    changed = tests.changed() => { if changed.is_err() { return; } },
                    _ = tick.tick() => {},
                }
                let brew = *tests.borrow_and_update();
                let active = brew.filter(|b| link.test_waits_for(*b));
                link.status.scale_live(
                    0.0,
                    active.map(|_| 0),
                    if active.is_some() {
                        ScalePhase::Waiting
                    } else {
                        ScalePhase::Idle
                    },
                    active,
                    active.is_some(),
                    false,
                );
            }
        })
    }
}

static SCALE: Kind = Kind {
    id: "test_scale",
    title: "Test scale",
    summary: "",
    icon: "",
    fields: &[],
    build: |_| Ok(Box::new(ReadyScale)),
};

#[tokio::test]
async fn scale_tests_are_selected_and_never_answer_or_abort_a_grinder_brew() {
    let (base, x) = start_server().await;
    x.record_history(None).unwrap();
    x.integrations()
        .start_cli(&x, &SCALE, &Settings::new())
        .unwrap();
    x.integrations()
        .start_cli(&x, &SCALE, &Settings::new())
        .unwrap();
    let status = x.integrations().status("test_scale").unwrap();
    let other = x.integrations().status("test_scale-2").unwrap();
    wait_for("test scale ready", || {
        status.can_test_scale() && other.can_test_scale()
    })
    .await;
    let mut grinder = Grinder::new(&base);
    grinder.poll().await;
    let (code, _) = call(
        reqwest::Method::POST,
        format!("{base}/api/integrations/test_scale/test"),
        None,
    )
    .await;
    assert_eq!(code, 200);
    let brew = status.snapshot().test_brew.unwrap();
    assert!(other.snapshot().test_brew.is_none());
    wait_for("test live readings", || {
        status.snapshot().live.is_some_and(|v| v.brew == Some(brew))
    })
    .await;
    let state: Value = grinder.get("/api/state").await;
    assert_eq!(state["shot"]["test_integration_id"], "test_scale");
    assert_eq!(state["integrations"][0]["live"]["for_current_shot"], true);
    assert_eq!(state["integrations"][1]["live"]["for_current_shot"], false);
    assert_eq!(
        call(
            reqwest::Method::DELETE,
            format!("{base}/api/integrations/test_scale-2/test"),
            None
        )
        .await
        .0,
        409
    );
    let link = Link {
        server: x.clone(),
        status: status.clone(),
    };
    assert!(matches!(
        link.finish_test(brew, Duration::from_secs(25), 36.0),
        ReportOutcome::Reported { .. }
    ));
    assert_eq!(status.snapshot().reports, 0);
    for _ in 0..20 {
        if grinder.poll().await.status == MachineStatus::On {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        grinder.accepted, None,
        "a test is a flush, never an adjustment"
    );
    assert!(
        x.history().list().is_empty(),
        "tests stay out of grinder history"
    );
    wait_for("scale ready for another test", || status.can_test_scale()).await;
    assert_eq!(
        call(
            reqwest::Method::POST,
            format!("{base}/api/integrations/test_scale/test"),
            None
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            reqwest::Method::DELETE,
            format!("{base}/api/integrations/test_scale/test"),
            None
        )
        .await
        .0,
        200
    );
    assert!(matches!(
        x.with(|m, now| m.phase(now)),
        Phase::Finishing { aborted: true, .. }
    ));
    grinder.model.request_start();
    grinder
        .post(PATH_SCRIPTS_EXECUTE, json!({"ID": START_BREW_SCRIPT_ID}))
        .await;
    let current = link.waiting_brew().unwrap();
    assert_eq!(
        link.finish_test(brew, Duration::from_secs(25), 36.0),
        ReportOutcome::NotWaiting
    );
    assert_eq!(
        call(
            reqwest::Method::DELETE,
            format!("{base}/api/integrations/test_scale/test"),
            None
        )
        .await
        .0,
        409
    );
    assert_eq!(
        call(
            reqwest::Method::POST,
            format!("{base}/api/integrations/test_scale/test"),
            None
        )
        .await
        .0,
        409
    );
    assert!(link.waits_for(current));
    let state: Value = grinder.get("/api/state").await;
    assert!(state["shot"]["test_integration_id"].is_null());
    assert_eq!(state["integrations"][0]["live"]["for_current_shot"], false);
}

#[tokio::test]
async fn discovery_checks_input_and_shot_state_before_using_bluetooth() {
    let (base, x) = start_server().await;
    let (code, body) = call(
        reqwest::Method::POST,
        format!("{base}/api/bluetooth/scan"),
        Some(json!({"adapter":"../hci0"})),
    )
    .await;
    assert_eq!(code, 400);
    assert_eq!(body["code"], "bluetooth_adapter_invalid");
    x.with(|m, now| m.start_shot(now).unwrap());
    let (code, body) = call(
        reqwest::Method::POST,
        format!("{base}/api/bluetooth/scan"),
        Some(json!({"adapter":"hci0"})),
    )
    .await;
    assert_eq!(code, 409);
    assert_eq!(body["code"], "bluetooth_scan_busy");
    let refused = crate::http::client()
        .post(format!("{base}/api/bluetooth/scan"))
        .header("origin", "https://outside.example")
        .json(&json!({"adapter":"hci0"}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);
}
