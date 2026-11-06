//! Command-line flags for the BOOKOO integration.

use crate::integration::Settings;
pub(crate) use crate::integration::bluetooth_scale::config::{Config, FIELDS};

#[derive(Debug, Clone, clap::Args)]
#[group(id = "bookoo_bluetooth")]
#[command(next_help_heading = "BOOKOO Bluetooth scale")]
pub(crate) struct Args {
    /// Enable the BOOKOO Themis Mini at this Bluetooth address (Linux only).
    #[arg(long, env = "BOOKOO_ADDRESS")]
    bookoo_address: Option<String>,
    /// Linux Bluetooth adapter to use.
    #[arg(long, env = "BOOKOO_ADAPTER", default_value = "hci0")]
    bookoo_adapter: String,
    /// Optional Linea Mini address for tare and timing from its paddle.
    #[arg(long, env = "BOOKOO_MINI_ADDRESS")]
    bookoo_mini_address: Option<String>,
    /// Local Mini Bluetooth token (not an account password).
    #[arg(long, env = "BOOKOO_MINI_TOKEN", hide_env_values = true)]
    bookoo_mini_token: Option<String>,
}

impl Args {
    pub(crate) fn settings(&self) -> Option<Settings> {
        Some(
            Settings::new()
                .with("address", Some(self.bookoo_address.as_ref()?))
                .with("adapter", Some(&self.bookoo_adapter))
                .with("mini_address", self.bookoo_mini_address.as_ref())
                .with("mini_token", self.bookoo_mini_token.as_ref()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_the_selected_device_and_adapter() {
        let settings = Settings::new().with("address", Some("aa:bb:cc:dd:ee:ff"));
        let c = Config::from_settings(&settings).unwrap();
        assert_eq!(c.address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(c.adapter, "hci0");
        for bad in [
            "",
            "any",
            "AA:BB:CC:DD:EE",
            "GG:BB:CC:DD:EE:FF",
            "AA:BB:CC:DD:EE:FFF",
        ] {
            assert!(Config::from_settings(&Settings::new().with("address", Some(bad))).is_err());
        }
        for bad in ["hci", "hci-1", "hci0/other", "../hci0"] {
            assert!(Config::from_settings(&settings.clone().with("adapter", Some(bad))).is_err());
        }
        let args = Args {
            bookoo_address: Some(c.address),
            bookoo_adapter: "hci1".into(),
            bookoo_mini_address: None,
            bookoo_mini_token: None,
        };
        assert_eq!(
            Config::from_settings(&args.settings().unwrap())
                .unwrap()
                .adapter,
            "hci1"
        );
    }
}
