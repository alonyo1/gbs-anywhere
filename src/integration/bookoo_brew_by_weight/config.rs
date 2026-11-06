//! Explicit setup for the experimental native brew-by-weight bridge.
pub(crate) use crate::integration::bluetooth_scale::config::Config;
use crate::integration::{Field, Input, Settings};
use anyhow::Context;
pub(super) const FIELDS: &[Field] = &[
    Field {
        key: "address",
        label: "BOOKOO Mini Bluetooth address",
        help: "Use the scale scan to select your BOOKOO Themis Mini. Use manual weighing and timer mode, in grams.",
        input: Input::Text,
        required: true,
        default: "",
    },
    Field {
        key: "adapter",
        label: "Bluetooth adapter",
        help: "Usually hci0. The Linux adapter must support simultaneous central and peripheral connections.",
        input: Input::Text,
        required: false,
        default: "hci0",
    },
    Field {
        key: "mini_address",
        label: "Linea Mini Bluetooth address",
        help: "Select the Mini that will receive the virtual scale. Disable the ordinary BOOKOO integration before using this bridge.",
        input: Input::Text,
        required: true,
        default: "",
    },
    Field {
        key: "mini_token",
        label: "Linea Mini Bluetooth token",
        help: "Local Bluetooth token for registering the virtual scale. Read it in pairing mode with --read-mini-token ADDRESS. No account password is needed.",
        input: Input::Password,
        required: true,
        default: "",
    },
];
pub(super) fn parse(s: &Settings) -> anyhow::Result<Config> {
    let c = Config::from_settings(s)?;
    c.mini
        .as_ref()
        .context("Mini address and Bluetooth token are required for the bridge")?;
    Ok(c)
}
#[derive(Debug, Clone, clap::Args)]
#[group(id = "bookoo_brew_by_weight")]
#[command(next_help_heading = "BOOKOO brew-by-weight bridge (experimental)")]
pub(crate) struct Args {
    /// Enable the experimental BOOKOO Mini bridge at this scale address.
    #[arg(long, env = "BOOKOO_BBW_ADDRESS")]
    bookoo_bbw_address: Option<String>,
    /// Linux adapter with simultaneous central/peripheral support.
    #[arg(long, env = "BOOKOO_BBW_ADAPTER", default_value = "hci0")]
    bookoo_bbw_adapter: String,
    /// Mini that will receive the virtual scale.
    #[arg(long, env = "BOOKOO_BBW_MINI_ADDRESS")]
    bookoo_bbw_mini_address: Option<String>,
    /// Local Mini Bluetooth token for scale registration.
    #[arg(long, env = "BOOKOO_BBW_MINI_TOKEN", hide_env_values = true)]
    bookoo_bbw_mini_token: Option<String>,
}
impl Args {
    pub(crate) fn settings(&self) -> Option<Settings> {
        Some(
            Settings::new()
                .with("address", Some(self.bookoo_bbw_address.as_ref()?))
                .with("adapter", Some(&self.bookoo_bbw_adapter))
                .with("mini_address", self.bookoo_bbw_mini_address.as_ref())
                .with("mini_token", self.bookoo_bbw_mini_token.as_ref()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_is_explicit_and_never_returns_its_token() {
        let s = Settings::new().with("address", Some("AA:BB:CC:DD:EE:01"));
        assert!(parse(&s).is_err());
        let s = s
            .with("mini_address", Some("AA:BB:CC:DD:EE:02"))
            .with("mini_token", Some("test-local-token"));
        assert!(parse(&s).is_ok());
        let (shown, secrets) = super::super::KIND.shown(&s);
        assert!(!shown.contains_key("mini_token"));
        assert_eq!(secrets, ["mini_token"]);
        assert!(!format!("{:?}", parse(&s).unwrap()).contains("test-local-token"));
        assert!(
            crate::integration::bookoo_bluetooth::KIND
                .create(&s, true)
                .is_ok()
        );
    }
}
