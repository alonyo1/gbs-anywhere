//! Paddle duration and settled cup weight. Pre-infusion pauses never finish
//! a shot: only a Mini stop event allows the weight to be finalized.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::bail;

use crate::integration::scale_shot::{Reading, Shot};
use crate::integration::tenths;

pub(crate) struct Capture {
    started: Instant,
    stopped: Option<(Instant, Duration)>,
    zero: bool,
    min_grams: f64,
    weights: VecDeque<Reading>,
}

impl Capture {
    pub(crate) fn new(started: Instant, recipe: Option<f64>) -> Self {
        Self {
            started,
            stopped: None,
            zero: false,
            min_grams: 5.0_f64.max(recipe.unwrap_or(0.0) * 0.5),
            weights: VecDeque::new(),
        }
    }

    pub(crate) fn stop(&mut self, at: Instant, time: Option<Duration>) -> anyhow::Result<()> {
        if at < self.started {
            bail!("Mini stop preceded its start; enter this shot manually");
        }
        if self.stopped.is_none() {
            let measured = at.duration_since(self.started);
            let time = time.unwrap_or(measured);
            // An old or inconsistent final timer must not answer a fresh grind.
            if time.abs_diff(measured) > Duration::from_secs(2) {
                bail!("Mini shot time disagreed with its live events; enter this shot manually");
            }
            if time < Duration::from_secs(5) {
                bail!("Mini shot was too short; enter this shot manually");
            }
            self.stopped = Some((at, time));
        }
        Ok(())
    }

    pub(crate) fn time_ms(&self, now: Instant) -> u64 {
        self.stopped
            .map_or_else(|| now.saturating_duration_since(self.started), |(_, t)| t)
            .as_millis() as u64
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopped.is_some()
    }

    pub(crate) fn read(&mut self, r: Reading, now: Instant) -> anyhow::Result<Option<Shot>> {
        if !self.zero {
            if now.saturating_duration_since(self.started) > Duration::from_millis(1500) {
                bail!(
                    "scale did not confirm tare promptly after the paddle; enter this shot manually"
                );
            }
            if r.grams.abs() > 0.5 {
                return Ok(None);
            }
            self.zero = true;
        }
        // A removed cup cannot provide the final yield. Keep manual entry
        // available instead of reporting a weight from before the last drips.
        if r.grams < -2.0 {
            bail!("cup was removed before its final weight settled; enter this shot manually");
        }
        self.weights.push_back(r);
        while self.weights.len() > 1000
            || self
                .weights
                .front()
                .is_some_and(|v| v.ms.saturating_add(3200) < r.ms)
        {
            self.weights.pop_front();
        }
        let Some((stopped, time)) = self.stopped else {
            return Ok(None);
        };
        if now.saturating_duration_since(stopped) < Duration::from_secs(3) {
            return Ok(None);
        }
        let from = r.ms.saturating_sub(3000);
        if self.weights.front().is_none_or(|v| v.ms > from) {
            return Ok(None);
        }
        let mut values: Vec<f64> = self
            .weights
            .iter()
            .filter(|v| v.ms >= from)
            .map(|v| v.grams)
            .collect();
        if values.len() < 3 {
            return Ok(None);
        }
        values.sort_by(f64::total_cmp);
        // Ignore a single radio/sample spike at either end of the window.
        let weight = values[values.len() / 2];
        if values[values.len() - 2] - values[1] > 0.5 || weight < self.min_grams {
            return Ok(None);
        }
        Ok(Some(Shot {
            time,
            grams: tenths(weight),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn starts_at_paddle_waits_for_stop_and_keeps_drips_out_of_time() {
        let start = Instant::now();
        let mut c = Capture::new(start, Some(36.0));
        let mut result = None;
        for ms in (0..=35000).step_by(100) {
            let now = start + Duration::from_millis(ms);
            if ms == 28000 {
                c.stop(now, Some(Duration::from_secs(28))).unwrap();
            }
            // Seven seconds of preinfusion, a pause, then coffee and late drips.
            let grams = match ms {
                0..=7000 => 0.0,
                7001..=12000 => 10.0,
                12001..=28000 => 10.0 + (ms - 12000) as f64 * 26.0 / 16000.0,
                28001..=30000 => 36.0 + (ms - 28000) as f64 / 2000.0,
                _ => 37.0,
            };
            if result.is_none() {
                result = c.read(Reading { ms, grams }, now).unwrap();
            }
            if ms < 31000 {
                assert!(result.is_none());
            }
        }
        let s = result.unwrap();
        assert_eq!(s.time, Duration::from_secs(28));
        assert_eq!(s.grams, 37.0);
        assert_eq!(c.time_ms(start + Duration::from_secs(40)), 28000);
    }
    #[test]
    fn late_tare_cup_removal_and_inconsistent_time_are_rejected() {
        let now = Instant::now();
        let mut c = Capture::new(now, None);
        assert!(
            c.read(
                Reading {
                    ms: 2000,
                    grams: 0.0
                },
                now + Duration::from_secs(2)
            )
            .is_err()
        );
        let mut c = Capture::new(now, None);
        c.read(Reading { ms: 0, grams: 0.0 }, now).unwrap();
        assert!(
            c.read(
                Reading {
                    ms: 100,
                    grams: -150.0
                },
                now
            )
            .is_err()
        );
        assert!(
            c.stop(now + Duration::from_secs(30), Some(Duration::from_secs(10)))
                .is_err()
        );
    }
}
