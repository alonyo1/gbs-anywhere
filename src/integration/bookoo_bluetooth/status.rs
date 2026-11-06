//! Status wording for this integration.
use crate::integration::bluetooth_scale::Line;
use crate::integration::{Detail, tenths};

pub(crate) fn detail(line: Line) -> Detail {
    match line {
        Line::Paddle => Detail::keyed(
            "bookoo_bluetooth.paddle",
            "ready · lift the Mini paddle to tare and start timing",
        ),
        Line::Brewing => Detail::keyed(
            "bookoo_bluetooth.brewing",
            "Mini is brewing · timing from the paddle",
        ),
        Line::Draining => Detail::keyed(
            "bookoo_bluetooth.draining",
            "Mini stopped · waiting for the final cup weight",
        ),
        Line::Testing => Detail::keyed(
            "bookoo_bluetooth.testing",
            "measuring a test shot · no grinder adjustment",
        ),
        Line::Connecting => Detail::keyed(
            "bookoo_bluetooth.connecting",
            "connecting to the selected Bluetooth scale",
        ),
        Line::Idle(g) => Detail::keyed(
            "bookoo_bluetooth.idle",
            format!(
                "ready for the next grind · scale reading {:.1} g",
                tenths(g)
            ),
        )
        .var("weight_g", tenths(g)),
        Line::LastShot(s) => Detail::keyed(
            "bookoo_bluetooth.last_shot",
            format!("last shot: {:.1} s, {:.1} g", s.time.as_secs_f64(), s.grams),
        )
        .var("time_ms", s.time.as_millis() as u64)
        .var("weight_g", s.grams),
        Line::Settling => Detail::keyed(
            "bookoo_bluetooth.settling",
            "grinder is waiting · waiting for a steady weight",
        ),
        Line::Ready => Detail::keyed(
            "bookoo_bluetooth.ready",
            "grinder is waiting · watching for the first drops",
        ),
        Line::Flowing(g) => Detail::keyed(
            "bookoo_bluetooth.flowing",
            format!("flowing · {:.1} g", tenths(g)),
        )
        .var("weight_g", tenths(g)),
        Line::Late => Detail::keyed(
            "bookoo_bluetooth.late",
            "scale connected after the grind · enter this shot manually",
        ),
    }
}
