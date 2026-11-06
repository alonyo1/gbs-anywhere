//! Scale readings for the app, with server ages and the brew they belong to.

use std::time::Instant;

use serde::Serialize;

use super::Brew;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScalePhase {
    Idle,
    Waiting,
    Paddle,
    Brewing,
    Draining,
    Flowing,
    Done,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScaleLive {
    pub(crate) weight_g: f64,
    pub(crate) time_ms: Option<u64>,
    pub(crate) phase: ScalePhase,
    pub(crate) test: bool,
    pub(crate) timing: &'static str,
    pub(crate) age_ms: u64,
    /// Filled by the control API against the current brew, never inferred
    /// from a phone's clock or from the order replies arrive.
    pub(crate) for_current_shot: bool,
    #[serde(skip)]
    pub(crate) brew: Option<Brew>,
}

#[derive(Debug)]
pub(crate) struct LiveState {
    value: ScaleLive,
    received: Instant,
}

impl LiveState {
    pub(crate) fn new(
        weight_g: f64,
        time_ms: Option<u64>,
        phase: ScalePhase,
        brew: Option<Brew>,
        test: bool,
        paddle_timing: bool,
    ) -> Self {
        Self {
            value: ScaleLive {
                weight_g,
                time_ms,
                phase,
                test,
                timing: if paddle_timing {
                    "paddle"
                } else {
                    "first_drops"
                },
                age_ms: 0,
                for_current_shot: false,
                brew,
            },
            received: Instant::now(),
        }
    }

    pub(super) fn mini_scale_commands(mut self) -> Self {
        self.value.timing = "mini_scale";
        self
    }

    pub(crate) fn snapshot(&self) -> ScaleLive {
        let mut value = self.value.clone();
        value.age_ms = self.received.elapsed().as_millis() as u64;
        value
    }
}
