//! Experimental BOOKOO Mini -> Linux -> Linea Mini native scale bridge.
pub(crate) mod config;
#[cfg(target_os = "linux")]
mod gatt;
mod recording;
#[cfg(target_os = "linux")]
mod registration;
mod run;
#[cfg(target_os = "linux")]
mod source;
mod wire;
use crate::integration::{Integration, Kind, Settings};
pub(crate) const TITLE: &str = "BOOKOO → Mini brew-by-weight";
pub(crate) static KIND: Kind = Kind {
    id: "bookoo_brew_by_weight",
    title: TITLE,
    summary: "Experimental Bluetooth bridge. Set the target yield in the La Marzocco app; the Mini stops the shot. Requires Linux and a BOOKOO Themis Mini. Physical compatibility and stopping accuracy are not yet verified. Use only this integration for the scale and Mini.",
    icon: include_str!("icon.svg"),
    fields: config::FIELDS,
    build,
};
fn build(settings: &Settings) -> anyhow::Result<Box<dyn Integration>> {
    Ok(Box::new(run::Bridge {
        cfg: config::parse(settings)?,
    }))
}
