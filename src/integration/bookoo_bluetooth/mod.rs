//! Reports shot measurements locally over Linux Bluetooth.

pub(crate) mod config;
pub(crate) mod protocol;
mod status;

use crate::integration::bluetooth_scale::{Model, run::BluetoothScale};
use crate::integration::{Integration, Kind, Settings};

pub(crate) const TITLE: &str = "BOOKOO Themis Mini";

pub(crate) static KIND: Kind = Kind {
    id: "bookoo_bluetooth",
    title: TITLE,
    summary: "Experimental. Verify the live reading with a known weight before using GbS. Time starts at first drops or optionally at the Mini paddle. Stop the machine yourself. Requires Linux Bluetooth.",
    icon: include_str!("icon.svg"),
    fields: config::FIELDS,
    build,
};

fn build(settings: &Settings) -> anyhow::Result<Box<dyn Integration>> {
    Ok(Box::new(BluetoothScale {
        cfg: config::Config::from_settings(settings)?,
        model: Model::Bookoo,
    }))
}

pub(crate) use status::detail;
