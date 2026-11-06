//! The setup form and command-line flags.

use anyhow::{Context, bail};

use crate::integration::{Field, Input, Settings};

pub(crate) const FIELDS: &[Field] = &[
    Field {
        key: "address",
        label: "Scale Bluetooth address",
        help: "The scale's Bluetooth address, e.g. AA:BB:CC:DD:EE:FF. \
               Find it with bluetoothctl on the computer running gbs-anywhere.",
        input: Input::Text,
        required: true,
        default: "",
    },
    Field {
        key: "adapter",
        label: "Bluetooth adapter",
        help: "Usually hci0. The adapter must be powered on in Linux.",
        input: Input::Text,
        required: false,
        default: "hci0",
    },
    Field {
        key: "mini_address",
        label: "Linea Mini Bluetooth address",
        help: "Optional paddle timing. Arm GbS first, then lift the Mini's paddle to tare and start timing. Leave blank for timing from first drops.",
        input: Input::Text,
        required: false,
        default: "",
    },
    Field {
        key: "mini_token",
        label: "Linea Mini Bluetooth token",
        help: "Required with a Mini address. A local Bluetooth token, not your account password. Read it in pairing mode with --read-mini-token ADDRESS.",
        input: Input::Password,
        required: false,
        default: "",
    },
];

#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) address: String,
    pub(crate) adapter: String,
    pub(crate) mini: Option<MiniConfig>,
}

#[derive(Clone)]
pub(crate) struct MiniConfig {
    pub(crate) address: String,
    pub(crate) token: String,
}

impl std::fmt::Debug for MiniConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MiniConfig")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Config {
    pub(crate) fn from_settings(s: &Settings) -> anyhow::Result<Self> {
        let address = s
            .text("address")
            .context("Scale Bluetooth address is required")?;
        let address = device_address(address)?;
        let adapter = adapter_name(s.text("adapter").unwrap_or("hci0"))?;
        let mini = s
            .text("mini_address")
            .map(|a| -> anyhow::Result<MiniConfig> {
                let address = device_address(a)?;
                let token = s
                    .secret("mini_token")
                    .context("Linea Mini Bluetooth token is required with a Mini address")?;
                validate_token(token)?;
                Ok(MiniConfig {
                    address,
                    token: token.to_owned(),
                })
            })
            .transpose()?;
        if mini.as_ref().is_some_and(|m| m.address == address) {
            bail!("the Mini and scale must have different Bluetooth addresses");
        }
        Ok(Self {
            address,
            adapter,
            mini,
        })
    }
}

pub(crate) fn validate_token(token: &str) -> anyhow::Result<()> {
    if token.is_empty()
        || token.len() > 512
        || token.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        bail!("invalid Mini Bluetooth token; read it again in pairing mode");
    }
    Ok(())
}

pub(crate) fn device_address(address: &str) -> anyhow::Result<String> {
    let parts: Vec<_> = address.split(':').collect();
    if parts.len() != 6
        || parts
            .iter()
            .any(|p| p.len() != 2 || !p.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        bail!("enter a Bluetooth address as six pairs, e.g. AA:BB:CC:DD:EE:FF");
    }
    Ok(address.to_ascii_uppercase())
}

pub(crate) fn adapter_name(adapter: &str) -> anyhow::Result<String> {
    if !adapter
        .strip_prefix("hci")
        .is_some_and(|n| !n.is_empty() && n.len() <= 5 && n.bytes().all(|b| b.is_ascii_digit()))
    {
        bail!("enter a Linux Bluetooth adapter name, e.g. hci0");
    }
    Ok(adapter.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_keep_first_drop_timing_and_mini_secrets_are_hidden() {
        let original = Settings::new().with("address", Some("AA:BB:CC:DD:EE:01"));
        assert!(Config::from_settings(&original).unwrap().mini.is_none());
        let settings = original.with("mini_address", Some("AA:BB:CC:DD:EE:02"));
        assert!(Config::from_settings(&settings).is_err());
        let settings = settings.with("mini_token", Some("sample-local-token"));
        let config = Config::from_settings(&settings).unwrap();
        assert!(!format!("{config:?}").contains("sample-local-token"));
        let (shown, secrets) = crate::integration::bookoo_bluetooth::KIND.shown(&settings);
        assert!(!shown.contains_key("mini_token"));
        assert_eq!(secrets, ["mini_token"]);
        assert!(
            Config::from_settings(
                &settings
                    .clone()
                    .with("mini_address", Some("AA:BB:CC:DD:EE:01"))
            )
            .is_err()
        );
        assert!(
            Config::from_settings(&settings.clone().with("mini_token", Some("bad\nsecret")))
                .is_err()
        );
        let off = settings.with("mini_address", Some(""));
        assert!(Config::from_settings(&off).unwrap().mini.is_none());
    }
}
