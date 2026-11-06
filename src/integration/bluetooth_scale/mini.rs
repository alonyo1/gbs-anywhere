//! Local Mini brew notifications. Only an explicit, fresh start may tare;
//! telemetry and an in-progress timer can never start a measurement.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::Value;

#[cfg(target_os = "linux")]
#[path = "mini_bluez.rs"]
pub(crate) mod bluez;

#[derive(Debug, Clone, Copy)]
pub(super) enum Event {
    Started { at: Instant },
    Stopped { at: Instant, time: Option<Duration> },
    Cancelled,
}

pub(super) trait Events: Send {
    fn next(&mut self) -> impl std::future::Future<Output = anyhow::Result<Event>> + Send;
}

/// Used by the ordinary first-drop workflow; it never produces Mini events.
pub(super) struct NoMini;
impl Events for NoMini {
    async fn next(&mut self) -> anyhow::Result<Event> {
        std::future::pending().await
    }
}

pub(super) struct Decoder {
    replay: bool,
    running: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            replay: true,
            running: false,
        }
    }
}

impl Decoder {
    pub(super) fn feed(&mut self, bytes: &[u8], at: Instant) -> anyhow::Result<Option<Event>> {
        // The machine replays its last notification on every subscription.
        // It can be hours old, including a start. Never act on it.
        if std::mem::take(&mut self.replay) {
            return Ok(None);
        }
        if bytes.len() > 4096 {
            bail!("Mini sent an oversized Bluetooth notification");
        }
        let start = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
        let end = bytes.iter().rposition(|b| *b != 0).map_or(start, |i| i + 1);
        let v: Value = serde_json::from_slice(&bytes[start..end])
            .map_err(|_| anyhow::anyhow!("Mini sent an invalid Bluetooth notification"))?;
        let v = v
            .as_object()
            .context("Mini Bluetooth notification is not an object")?;
        let has = |k: &str| v.get(k).is_some_and(|v| !v.is_null());
        let flag = |k: &str| {
            v.get(k).and_then(|v| {
                v.as_bool().or_else(|| match v.as_str()? {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                })
            })
        };
        if flag("BrewingUpdateGroup1Backflush") == Some(true)
            || has("FlushStoppedGroup1Time")
            || has("FlushStoppedGroup1DoseIndex")
        {
            self.running = false;
            return Ok(Some(Event::Cancelled));
        }
        if has("BrewingStoppedGroup1Time")
            || has("BrewingStoppedGroup1DoseIndex")
            || has("BrewingStoppedGroup1StopType")
            || has("BrewingStoppedGroup1StopReason")
        {
            self.running = false;
            let time = v
                .get("BrewingStoppedGroup1Time")
                .filter(|v| !v.is_null())
                .map(|v| {
                    let seconds = v
                        .as_f64()
                        .or_else(|| v.as_str()?.parse().ok())
                        .context("Mini sent an invalid shot time")?;
                    if !seconds.is_finite() || !(0.0..=3600.0).contains(&seconds) {
                        bail!("Mini sent an invalid shot time");
                    }
                    Ok(Duration::from_secs_f64(seconds))
                })
                .transpose()?;
            return Ok(Some(Event::Stopped { at, time }));
        }
        if has("BrewingStartedGroup1DoseIndex") {
            if self.running {
                return Ok(None);
            }
            self.running = true;
            // A start accompanied by an elapsed timer is not a fresh start.
            if let Some(t) = v.get("BrewingUpdateGroup1Time") {
                let seconds = t.as_f64().or_else(|| t.as_str()?.parse().ok());
                if !seconds.is_some_and(|n| (0.0..=0.5).contains(&n)) {
                    return Ok(None);
                }
            }
            return Ok(Some(Event::Started { at }));
        }
        // Timer updates alone mean we joined mid-shot. Even if a delayed
        // start follows them, don't tare a cup that may already contain coffee.
        if flag("BrewingUpdateGroup1Ev") == Some(true)
            || v.get("BrewingUpdateGroup1Time")
                .and_then(|t| t.as_f64().or_else(|| t.as_str()?.parse().ok()))
                .is_some_and(|seconds| seconds > 0.0)
        {
            self.running = true;
        } else if flag("BrewingUpdateGroup1Ev") == Some(false) {
            self.running = false;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn feed(d: &mut Decoder, v: Value) -> Option<Event> {
        d.feed(&serde_json::to_vec(&v).unwrap(), Instant::now())
            .unwrap()
    }
    #[test]
    fn ignores_replay_telemetry_midshot_and_duplicate_starts() {
        let mut d = Decoder::default();
        let start = serde_json::json!({"BrewingStartedGroup1DoseIndex":"Continuous"});
        assert!(feed(&mut d, start.clone()).is_none());
        assert!(
            feed(
                &mut d,
                serde_json::json!({"CoffeeBoiler1UpdateTemperature":93})
            )
            .is_none()
        );
        assert!(matches!(
            feed(&mut d, start.clone()),
            Some(Event::Started { .. })
        ));
        assert!(feed(&mut d, start.clone()).is_none());
        assert!(
            matches!(feed(&mut d, serde_json::json!({"BrewingStoppedGroup1Time":"28.5"})), Some(Event::Stopped { time: Some(t), .. }) if t == Duration::from_millis(28500))
        );
        assert!(
            feed(
                &mut d,
                serde_json::json!({"BrewingUpdateGroup1Ev":true,"BrewingUpdateGroup1Time":10})
            )
            .is_none()
        );
        assert!(feed(&mut d, start).is_none());
        assert!(matches!(
            feed(&mut d, serde_json::json!({"FlushStoppedGroup1Time":3})),
            Some(Event::Cancelled)
        ));
    }
    #[test]
    fn idle_timer_reset_does_not_block_the_next_paddle() {
        let mut d = Decoder::default();
        feed(&mut d, serde_json::json!({}));
        feed(&mut d, serde_json::json!({"BrewingUpdateGroup1Time":0}));
        assert!(matches!(
            feed(
                &mut d,
                serde_json::json!({"BrewingStartedGroup1DoseIndex":"Continuous"})
            ),
            Some(Event::Started { .. })
        ));
    }
    #[test]
    fn malformed_data_and_invalid_times_never_become_shots() {
        for packet in [
            b"garbage".as_slice(),
            b"[]",
            b"{\"BrewingStoppedGroup1Time\":-1}",
            b"{\"BrewingStoppedGroup1Time\":\"NaN\"}",
        ] {
            let mut d = Decoder {
                replay: false,
                running: true,
            };
            assert!(d.feed(packet, Instant::now()).is_err());
        }
    }
}
