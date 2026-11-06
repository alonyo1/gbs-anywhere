//! GbS reporting is independent of scale forwarding: a reporting error must
//! not interrupt native brew-by-weight while measurements remain valid.
use super::wire::{Action, Sample};
use crate::integration::bluetooth_scale::paddle::Capture;
use crate::integration::scale_shot::Reading;
use crate::integration::{Brew, Detail, Link, ScalePhase};
use std::time::{Duration, Instant};

pub(super) struct Recording {
    blocked: Option<Brew>,
    active: Option<(Brew, bool, Capture)>,
    tests: tokio::sync::watch::Receiver<Option<Brew>>,
    error: Option<String>,
    started: Instant,
}
impl Recording {
    pub fn new(link: &Link) -> Self {
        let mut tests = link.status.scale_tests();
        tests.borrow_and_update();
        Self {
            blocked: link.waiting_brew(),
            active: None,
            tests,
            error: None,
            started: Instant::now(),
        }
    }
    fn requested(&self, link: &Link) -> Option<(Brew, bool)> {
        if let Some(b) = *self.tests.borrow()
            && link.test_waits_for(b)
        {
            return Some((b, true));
        }
        link.waiting_brew()
            .filter(|b| Some(*b) != self.blocked)
            .map(|b| (b, false))
    }
    pub fn event(&mut self, action: Action, at: Instant, link: &Link) {
        if self.active.as_ref().is_some_and(|(b, t, _)| {
            !(if *t {
                link.test_waits_for(*b)
            } else {
                link.waits_for(*b)
            })
        }) {
            self.active = None;
        }
        let result = (|| -> anyhow::Result<()> {
            match action {
                Action::Start => {
                    anyhow::ensure!(
                        self.active.is_none(),
                        "Mini restarted before the previous yield settled; enter this shot manually"
                    );
                    if let Some((b, test)) = self.requested(link) {
                        if at < b.started_at() {
                            return Ok(());
                        }
                        anyhow::ensure!(
                            at.elapsed() <= Duration::from_millis(500),
                            "Mini timer start arrived late; enter this shot manually"
                        );
                        let target = if test {
                            None
                        } else {
                            link.server.with(|m, _| m.recipe_weight_g())
                        };
                        self.active = Some((b, test, Capture::new(at, target)));
                        self.error = None;
                    }
                }
                Action::Stop => {
                    if let Some((_, _, c)) = &mut self.active {
                        c.stop(at, None)?;
                    }
                }
                Action::Reset | Action::Tare => anyhow::ensure!(
                    self.active.is_none(),
                    "Mini reset the scale during a shot; enter this shot manually"
                ),
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.fail(e.to_string());
        }
    }
    fn fail(&mut self, error: String) {
        if let Some((b, _, _)) = self.active.take() {
            self.blocked = Some(b);
        }
        self.error = Some(error);
    }
    pub fn reading(
        &mut self,
        s: Sample,
        now: Instant,
        linked: bool,
        registration_error: Option<&str>,
        link: &Link,
    ) {
        if self.active.as_ref().is_some_and(|(b, t, _)| {
            !(if *t {
                link.test_waits_for(*b)
            } else {
                link.waits_for(*b)
            })
        }) {
            self.active = None;
        }
        if let Some((brew, test, c)) = &mut self.active {
            let result = c.read(
                Reading {
                    ms: now.saturating_duration_since(self.started).as_millis() as u64,
                    grams: s.grams,
                },
                now,
            );
            match result {
                Ok(Some(shot)) => {
                    if *test {
                        link.finish_test(*brew, shot.time, shot.grams);
                    } else {
                        link.report(
                            *brew,
                            shot.time,
                            Some((shot.grams, "weighed by the BOOKOO bridge".into())),
                        );
                    }
                    self.active = None;
                }
                Err(e) => self.fail(e.to_string()),
                Ok(None) => {}
            }
        }
        let (phase, time, brew, test) = if let Some((b, t, c)) = &self.active {
            (
                if c.stopped() {
                    ScalePhase::Draining
                } else {
                    ScalePhase::Brewing
                },
                Some(c.time_ms(now)),
                Some(*b),
                *t,
            )
        } else if let Some((b, t)) = self.requested(link) {
            (ScalePhase::Paddle, Some(0), Some(b), t)
        } else {
            (ScalePhase::Idle, None, None, false)
        };
        if let Some(error) = &self.error {
            link.status.error(error.clone());
        } else if !linked {
            if let Some(error) = registration_error {
                link.status.starting(error);
            } else {
                link.status.starting(Detail::keyed(
                    "bookoo_brew_by_weight.waiting",
                    "BOOKOO connected; waiting for the Mini to connect to LUNAR-C",
                ));
            }
        } else if phase == ScalePhase::Idle {
            link.status.connected(Detail::keyed(
                "bookoo_brew_by_weight.ready",
                "Bridge ready. Set the target yield in the La Marzocco app.",
            ));
        } else {
            link.status.watching(Detail::keyed(
                "bookoo_brew_by_weight.watching",
                "Following the Mini's scale timer; leave the cup until the final weight settles",
            ));
        }
        link.status
            .scale_bridge_live(s.grams, time, phase, brew, test);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::{ReportOutcome, Status};
    use crate::protocol::MachineConfig;
    use crate::server::Server;
    fn link() -> Link {
        Link {
            server: Server::new(MachineConfig::default()),
            status: Status::new(
                "bookoo_brew_by_weight",
                "bookoo_brew_by_weight",
                super::super::TITLE,
            ),
        }
    }
    fn shot(r: &mut Recording, l: &Link, at: Instant) {
        r.event(Action::Tare, at, l);
        r.event(Action::Start, at, l);
        for ms in (0..=34000).step_by(100) {
            let now = at + Duration::from_millis(ms);
            if ms == 28000 {
                r.event(Action::Stop, now, l);
            }
            let grams = if ms < 1000 {
                0.0
            } else if ms < 28000 {
                (ms - 1000) as f64 * 36.0 / 27000.0
            } else {
                37.0
            };
            r.reading(
                Sample {
                    grams,
                    timer: ms.min(28000) as u32,
                    battery: 80,
                },
                now,
                true,
                None,
                l,
            );
        }
    }
    #[test]
    fn native_scale_commands_report_only_fresh_grinder_requests_or_selected_tests() {
        for test in [false, true] {
            let l = link();
            let mut r = Recording::new(&l);
            l.server.with(|m, now| {
                if test {
                    m.start_shot(now).unwrap();
                    l.status.request_scale_test(now);
                } else {
                    m.on_start_request(now, Some(9));
                }
            });
            shot(&mut r, &l, Instant::now());
            let s = l.server.with(|m, _| m.last_shot()).unwrap();
            assert_eq!(s.time_ms, 28000);
            assert_eq!(s.weight_g, 37.0);
            assert_eq!(l.status.snapshot().reports, if test { 0 } else { 1 });
            assert_eq!(l.status.snapshot().live.unwrap().timing, "mini_scale");
        }
        let l = link();
        let mut r = Recording::new(&l);
        shot(&mut r, &l, Instant::now());
        assert!(l.server.with(|m, _| m.last_shot()).is_none());
        l.server.with(|m, now| {
            m.on_start_request(now, Some(9));
        });
        let mut reconnected = Recording::new(&l);
        shot(&mut reconnected, &l, Instant::now());
        assert!(l.server.with(|m, _| m.last_shot()).is_none());
    }
    #[test]
    fn replacement_test_abort_and_native_reset_cannot_report_old_weights() {
        let l = link();
        let mut r = Recording::new(&l);
        l.server.with(|m, now| {
            m.on_start_request(now, Some(9));
        });
        let at = Instant::now();
        r.event(Action::Start, at, &l);
        r.reading(
            Sample {
                grams: 0.0,
                timer: 0,
                battery: 80,
            },
            at,
            true,
            None,
            &l,
        );
        let old = l.waiting_brew().unwrap();
        l.server.with(|m, now| m.abort(now)).unwrap();
        l.server.with(|m, now| {
            m.on_start_request(now, Some(10));
        });
        r.event(Action::Stop, at + Duration::from_secs(28), &l);
        for ms in (28000..=32000).step_by(100) {
            r.reading(
                Sample {
                    grams: 36.0,
                    timer: 28000,
                    battery: 80,
                },
                at + Duration::from_millis(ms),
                true,
                None,
                &l,
            );
        }
        assert!(l.server.with(|m, _| m.last_shot()).is_none());
        assert!(matches!(
            l.report(old, Duration::from_secs(28), Some((36.0, "old".into()))),
            ReportOutcome::BrewOver
        ));
        r.event(Action::Start, Instant::now(), &l);
        r.event(Action::Reset, Instant::now(), &l);
        assert!(r.active.is_none());
        assert!(r.error.is_some());
    }
}
