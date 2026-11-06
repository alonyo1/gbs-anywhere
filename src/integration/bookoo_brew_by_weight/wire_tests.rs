use super::*;
fn sample(g: f64, t: u32) -> Sample {
    Sample {
        grams: g,
        timer: t,
        battery: 80,
    }
}
fn paired(now: Instant) -> Core {
    let mut c = Core::new(sample(150.0, 0), now);
    c.command(&packet(11, &[0; 15]), now).unwrap();
    assert_eq!(
        c.output(now).unwrap().unwrap(),
        packet(8, &[9, 80, 2, 16, 1, 0, 1, 1, 0])
    );
    c.delivered();
    c.command(&packet(12, &[9, 0, 1, 1, 2, 2, 5, 3, 4]), now)
        .unwrap();
    c
}
#[test]
fn fragments_and_concatenated_commands_are_bounded_and_checked() {
    let now = Instant::now();
    let hello = packet(11, &[0; 15]);
    for stride in 1..=hello.len() {
        let mut c = Core::new(sample(0.0, 0), now);
        for part in hello.chunks(stride) {
            c.command(part, now).unwrap();
        }
        assert!(c.paired);
    }
    let mut c = paired(now);
    let mut cmd = packet(4, &[0]);
    cmd.extend(packet(38, &[0, 0, 0]));
    c.command(&cmd, now).unwrap();
    assert_eq!(c.next_write(now).unwrap(), Some(Action::Tare));
    assert_eq!(c.event().unwrap().0, Action::Tare);
    assert_eq!(c.event().unwrap().0, Action::Start);
    let mut bad = packet(4, &[0]);
    *bad.last_mut().unwrap() ^= 1;
    assert!(c.command(&bad, now).is_err());
    assert!(paired(now).command(&[0; 257], now).is_err());
    assert!(paired(now).command(&[0xef, 0xdd, 12, 60], now).is_err());
    let mut c = paired(now);
    for a in [Action::Tare, Action::Reset, Action::Tare, Action::Reset] {
        c.enqueue(a, now).unwrap();
    }
    assert!(c.enqueue(Action::Stop, now).is_err());
}
#[test]
fn tare_ack_requires_a_new_zero_and_does_not_invent_weight() {
    let now = Instant::now();
    let mut c = paired(now);
    c.command(&packet(4, &[0]), now).unwrap();
    assert!(c.output(now).unwrap().is_none());
    assert_eq!(c.next_write(now).unwrap(), Some(Action::Tare));
    c.command(&packet(4, &[0]), now).unwrap();
    assert_eq!(c.controls.len(), 1);
    c.reading(sample(150.0, 0), now + Duration::from_millis(100))
        .unwrap();
    assert!(c.output(now).unwrap().is_none());
    c.reading(sample(0.0, 0), now + Duration::from_millis(200))
        .unwrap();
    assert_eq!(
        c.output(now).unwrap().unwrap(),
        packet(12, &[4, 11, 0, 0x80])
    );
    // Coffee can arrive while the ACK is queued: never replace it with zero.
    c.reading(sample(0.42, 0), now + Duration::from_millis(300))
        .unwrap();
    c.delivered();
    assert_eq!(
        c.output(now).unwrap().unwrap(),
        packet(12, &[8, 5, 42, 0, 0, 0, 2, 0])
    );
    c.delivered();
    assert!(c.output(now).unwrap().is_none());
}
#[test]
fn timers_reset_before_start_and_confirm_stop_from_separate_samples() {
    let now = Instant::now();
    let mut c = paired(now);
    c.command(&packet(38, &[0, 0, 0]), now).unwrap();
    assert_eq!(c.next_write(now).unwrap(), Some(Action::Reset));
    c.reading(sample(0.0, 20), now + Duration::from_millis(50))
        .unwrap();
    assert_eq!(c.next_write(now).unwrap(), None);
    c.reading(sample(0.0, 0), now + Duration::from_millis(100))
        .unwrap();
    assert_eq!(
        c.next_write(now + Duration::from_millis(100)).unwrap(),
        Some(Action::Start)
    );
    c.reading(sample(0.0, 0), now + Duration::from_millis(150))
        .unwrap();
    assert!(c.replies.is_empty());
    c.reading(sample(0.0, 100), now + Duration::from_millis(200))
        .unwrap();
    assert_eq!(c.output(now).unwrap().unwrap(), packet(12, &[4, 11, 0, 0]));
    c.delivered();
    c.command(&packet(38, &[0, 0, 1]), now + Duration::from_millis(250))
        .unwrap();
    assert_eq!(
        c.next_write(now + Duration::from_millis(250)).unwrap(),
        Some(Action::Stop)
    );
    for ms in [300, 400] {
        c.reading(sample(36.0, 200), now + Duration::from_millis(ms))
            .unwrap();
        assert!(c.replies.is_empty());
    }
    c.reading(sample(36.0, 200), now + Duration::from_millis(500))
        .unwrap();
    assert_eq!(
        c.output(now).unwrap().unwrap(),
        packet(12, &[4, 11, 0, 0x20])
    );
}
#[test]
fn stale_data_and_control_failure_cannot_be_hidden_by_a_late_reading() {
    let now = Instant::now();
    assert!(
        paired(now)
            .reading(sample(0.0, 0), now + Duration::from_millis(1001))
            .is_err()
    );
    let mut c = paired(now);
    c.command(&packet(4, &[0]), now).unwrap();
    c.next_write(now).unwrap();
    for ms in [500, 1000, 1500, 2000] {
        c.reading(sample(100.0, 0), now + Duration::from_millis(ms))
            .unwrap();
    }
    assert!(
        c.reading(sample(0.0, 0), now + Duration::from_millis(2001))
            .is_err()
    );
    assert!(
        Core::new(sample(0.0, 0), now)
            .command(&packet(4, &[0]), now)
            .is_err()
    );
    let c = paired(now);
    assert!(c.output(now + Duration::from_secs(2)).is_err());
}
#[test]
fn bookoo_decode_rejects_corruption_and_commands_match_published_frames() {
    let mut p = protocol::frame(-12.34);
    p[2..5].copy_from_slice(&[0x12, 0x34, 0x56]);
    p[13] = 80;
    p[19] = p[..19].iter().fold(0, |a, b| a ^ b);
    let s = Sample::decode(&p).unwrap();
    assert_eq!(s.grams, -12.34);
    assert_eq!(s.timer, 0x123456);
    assert_eq!(s.battery, 80);
    p[13] = 200;
    p[19] = p[..19].iter().fold(0, |a, b| a ^ b);
    assert!(Sample::decode(&p).is_err());
    assert_eq!(Action::Tare.bytes(), [3, 10, 1, 0, 0, 8]);
    assert_eq!(Action::Start.bytes(), [3, 10, 4, 0, 0, 10]);
    assert_eq!(Action::Stop.bytes(), [3, 10, 5, 0, 0, 13]);
    assert_eq!(Action::Reset.bytes(), [3, 10, 6, 0, 0, 12]);
}
