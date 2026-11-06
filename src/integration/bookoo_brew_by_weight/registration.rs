//! Local registration only. Never replace another scale or send brew/power
//! commands. The temporary owner connection is released after registration.
use crate::integration::bluetooth_scale::{
    bluez::open_device,
    config::MiniConfig,
    mini::bluez::{authenticate, characteristic},
};
use anyhow::{Context, ensure};
use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::Value;
use zbus::{Connection, Proxy};
const READ: &str = "0a0b7847-e12b-09a8-b04b-8e0922a9abab";
const WRITE: &str = "0b0b7847-e12b-09a8-b04b-8e0922a9abab";
async fn write(c: &Proxy<'_>, mut bytes: Vec<u8>) -> anyhow::Result<()> {
    bytes.push(0);
    tokio::time::timeout(
        Duration::from_secs(3),
        c.call::<_, _, ()>(
            "WriteValue",
            &(bytes, HashMap::from([("type", Value::from("request"))])),
        ),
    )
    .await
    .context("Mini request timed out")??;
    Ok(())
}
async fn read(c: &Proxy<'_>, key: &str) -> anyhow::Result<serde_json::Value> {
    write(c, key.as_bytes().to_vec()).await?;
    let bytes: Vec<u8> = tokio::time::timeout(
        Duration::from_secs(3),
        c.call("ReadValue", &(HashMap::<String, Value<'_>>::new(),)),
    )
    .await
    .context("Mini read timed out")??;
    ensure!(
        bytes.len() <= 16384,
        "Mini registration response is too large"
    );
    serde_json::from_slice(bytes.split(|b| *b == 0).next().unwrap_or_default())
        .context("invalid Mini registration response")
}
#[derive(Debug, PartialEq, Eq)]
enum Registration {
    Present,
    Missing,
    Other,
}
fn classify(value: &serde_json::Value) -> anyhow::Result<Registration> {
    let entries = value
        .as_array()
        .context("Mini returned an unknown scale registration format")?;
    let mut own = false;
    let mut other = false;
    for e in entries {
        let name = e
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .context("Mini returned an invalid scale registration")?;
        if name == "LUNAR-C" {
            own = true;
        } else {
            other = true;
        }
    }
    // Do not make any write when an unrelated scale is already present.
    Ok(if own {
        Registration::Present
    } else if other {
        Registration::Other
    } else {
        Registration::Missing
    })
}

pub(super) async fn register(
    cfg: MiniConfig,
    adapter: String,
    mut available: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let state = available.clone();
    let operation = async {
        let mut gatt = open_device(Connection::system().await?, &adapter, &cfg.address).await?;
        authenticate(&gatt, &cfg.token).await?;
        let result = request(&gatt, &state).await;
        gatt.cleanup.close().await;
        result
    };
    let result = tokio::select! {
        r=tokio::time::timeout(Duration::from_secs(35),operation)=>r.context("Mini scale registration timed out")?,
        _=async {let _=available.wait_for(|ready| !*ready).await;}=>return Ok(()),
    };
    result.map_err(|_| anyhow::anyhow!("Mini scale registration was not completed. Check its token, close other Mini connections, and remove any other scale registration in the La Marzocco app."))
}
async fn request(
    gatt: &crate::integration::bluetooth_scale::bluez::Gatt,
    state: &tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    ensure!(
        *state.borrow(),
        "bridge is no longer ready for registration"
    );
    let query = characteristic(gatt, READ).await?;
    let mode = read(&query, "machineMode").await?;
    ensure!(
        mode.as_str() == Some("BrewingMode"),
        "turn on the Mini before registering the virtual scale"
    );
    let scales = read(&query, "scales").await?;
    match classify(&scales)? {
        Registration::Present => return Ok(()),
        Registration::Other => anyhow::bail!("another scale is registered on the Mini"),
        Registration::Missing => {}
    }
    ensure!(
        *state.borrow(),
        "bridge is no longer ready for registration"
    );
    let command = characteristic(gatt, WRITE).await?;
    ensure!(
        *state.borrow(),
        "bridge is no longer ready for registration"
    );
    let request = serde_json::json!({
        "id": format!("gbs-scale-{}", uuid::Uuid::new_v4()),
        "name": "SetScalePosition",
        "parameter": {"scale": "LUNAR-C", "force": false},
    });
    write(&command, serde_json::to_vec(&request)?).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registrations_never_overwrite_other_scales_or_unknown_data() {
        assert_eq!(
            classify(&serde_json::json!([])).unwrap(),
            Registration::Missing
        );
        assert_eq!(
            classify(&serde_json::json!([{"name":"LUNAR-C"}])).unwrap(),
            Registration::Present
        );
        assert_eq!(
            classify(&serde_json::json!([{"name":"Another scale"}])).unwrap(),
            Registration::Other
        );
        for v in [
            serde_json::json!({}),
            serde_json::json!([{}]),
            serde_json::json!([{"name":""}]),
        ] {
            assert!(classify(&v).is_err());
        }
    }
}
