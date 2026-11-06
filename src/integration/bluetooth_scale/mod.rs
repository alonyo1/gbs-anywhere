//! Shared Linux Bluetooth transport and scale-based shot workflow.

#[cfg(target_os = "linux")]
pub(crate) mod bluez;
pub(crate) mod config;
pub(crate) mod mini;
pub(crate) mod paddle;
pub(crate) mod run;

#[derive(Debug, serde::Serialize)]
pub(crate) struct DiscoveredScale {
    pub(crate) address: String,
    pub(crate) name: Option<String>,
    pub(crate) rssi_dbm: Option<i16>,
    pub(crate) connected: bool,
}

/// Discovery never pairs with or connects to a device. The person chooses
/// an address, and the integration validates its GATT service on connection.
pub(crate) async fn discover(adapter: &str) -> anyhow::Result<Vec<DiscoveredScale>> {
    let adapter = config::adapter_name(adapter)?;
    #[cfg(target_os = "linux")]
    {
        bluez::discover(&adapter).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = adapter;
        anyhow::bail!("Bluetooth discovery requires Linux and BlueZ")
    }
}

use crate::integration::scale_shot::Shot;
use crate::integration::{Detail, bookoo_bluetooth as bookoo};

#[derive(Clone, Copy)]
pub(crate) enum Model {
    Bookoo,
}

pub(crate) enum Line {
    Connecting,
    Idle(f64),
    LastShot(Shot),
    Settling,
    Ready,
    Flowing(f64),
    Late,
    Testing,
    Paddle,
    Brewing,
    Draining,
}

impl Model {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Bookoo => bookoo::TITLE,
        }
    }
    pub(crate) fn detail(self, line: Line) -> Detail {
        match self {
            Self::Bookoo => bookoo::detail(line),
        }
    }
    pub(crate) fn service(self) -> &'static str {
        match self {
            Self::Bookoo => bookoo::protocol::SERVICE,
        }
    }
    pub(crate) fn weight(self) -> &'static str {
        match self {
            Self::Bookoo => bookoo::protocol::WEIGHT,
        }
    }
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::Bookoo => bookoo::protocol::COMMAND,
        }
    }
    pub(crate) fn tare(self) -> Vec<u8> {
        match self {
            Self::Bookoo => bookoo::protocol::TARE.to_vec(),
        }
    }
}

pub(crate) enum Decoder {
    Bookoo,
}

impl Decoder {
    pub(crate) fn new(model: Model) -> Self {
        match model {
            Model::Bookoo => Self::Bookoo,
        }
    }
    pub(crate) fn weights(&mut self, packet: &[u8]) -> anyhow::Result<Vec<f64>> {
        match self {
            Self::Bookoo => Ok(bookoo::protocol::grams(packet).into_iter().collect()),
        }
    }
}

pub(super) async fn read_mini_token(address: &str, adapter: &str) -> anyhow::Result<String> {
    let address = config::device_address(address)?;
    let adapter = config::adapter_name(adapter)?;
    #[cfg(target_os = "linux")]
    {
        mini::bluez::read_token(&address, &adapter).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (address, adapter);
        anyhow::bail!("Mini Bluetooth requires Linux and BlueZ")
    }
}

/// Reserve physical devices for one integration in this process.
/// A native bridge also reserves its adapter's virtual-scale identity.
static CLAIMS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();
pub(crate) struct Claim(Vec<String>);
#[cfg(target_os = "linux")]
pub(crate) async fn wait_claim(
    link: &crate::integration::Link,
    adapter: &str,
    addresses: &[&str],
) -> Claim {
    loop {
        match claim(adapter, addresses) {
            Ok(claim) => return claim,
            Err(error) => link.status.error(error.to_string()),
        }
        // Saving an edit cancels the previous task asynchronously. Wait until
        // its lease drops; a competing integration stays visibly unavailable.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
pub(crate) fn claim(adapter: &str, addresses: &[&str]) -> anyhow::Result<Claim> {
    let keys: Vec<_> = addresses.iter().map(|a| format!("{adapter}/{a}")).collect();
    let mut held = CLAIMS.get_or_init(Default::default).lock().unwrap();
    anyhow::ensure!(
        keys.iter().all(|k| !held.contains(k)),
        "Bluetooth device is already used by another integration; remove that integration before enabling this one"
    );
    held.extend(keys.iter().cloned());
    Ok(Claim(keys))
}
impl Drop for Claim {
    fn drop(&mut self) {
        let mut held = CLAIMS.get().unwrap().lock().unwrap();
        for key in &self.0 {
            held.remove(key);
        }
    }
}

#[cfg(test)]
mod claim_tests {
    #[test]
    fn competing_integrations_cannot_share_a_device_and_failed_claims_are_atomic() {
        let first = super::claim("hci9999", &["scale-a", "mini-a"]).unwrap();
        assert!(super::claim("hci9999", &["scale-b", "mini-a"]).is_err());
        let second = super::claim("hci9999", &["scale-b"]).unwrap();
        drop(first);
        assert!(super::claim("hci9999", &["scale-a", "mini-a"]).is_ok());
        drop(second);
    }
}
