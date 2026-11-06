//! Authenticated notifications over the host BlueZ connection. No cloud calls.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, Proxy};

use super::{Decoder, Event, Events};
use crate::integration::bluetooth_scale::bluez::{Gatt, open_device, proxy};
use crate::integration::bluetooth_scale::config::{MiniConfig, validate_token};

const CHAR: &str = "org.bluez.GattCharacteristic1";
const AUTH: &str = "0d0b7847-e12b-09a8-b04b-8e0922a9abab";
const TOKEN: &str = "0c0b7847-e12b-09a8-b04b-8e0922a9abab";
const SHOT: &str = "0e0b7847-e12b-09a8-b04b-8e0922a9abab";

pub(crate) async fn characteristic(gatt: &Gatt, uuid: &str) -> anyhow::Result<Proxy<'static>> {
    let prefix = format!("{}/", gatt.device_path);
    let path = gatt.objects.iter().find_map(|(path, interfaces)| {
        let properties = interfaces.get(CHAR)?;
        let id = <&str>::try_from(properties.get("UUID")?).ok()?;
        (path.as_str().starts_with(&prefix) && id.eq_ignore_ascii_case(uuid)).then_some(path)
    }).context("Mini Bluetooth service is missing; check the selected address, firmware and BlueZ service cache")?;
    Ok(proxy(&gatt.bus, path.as_str(), CHAR).await?)
}

async fn read(c: &Proxy<'_>) -> anyhow::Result<Vec<u8>> {
    Ok(c.call("ReadValue", &(HashMap::<&str, Value<'_>>::new(),))
        .await?)
}

pub(in crate::integration::bluetooth_scale) async fn read_token(
    address: &str,
    adapter: &str,
) -> anyhow::Result<String> {
    tokio::time::timeout(Duration::from_secs(35), async {
        let mut gatt = open_device(Connection::system().await?, adapter, address).await?;
        let c = characteristic(&gatt, TOKEN).await?;
        let bytes = read(&c).await.map_err(|_| anyhow::anyhow!("cannot read Mini token; put the machine in pairing mode and close the La Marzocco app"))?;
        let token = String::from_utf8(bytes).context("Mini token is not UTF-8")?;
        validate_token(&token)?;
        gatt.cleanup.close().await;
        Ok(token)
    }).await.context("reading the Mini token timed out")?
}

pub(crate) async fn authenticate(gatt: &Gatt, token: &str) -> anyhow::Result<()> {
    let auth = characteristic(gatt, AUTH).await?;
    let options = HashMap::from([("type", Value::from("request"))]);
    // Never include a token or a peer's authentication error text in logs.
    auth.call::<_, _, ()>("WriteValue", &(token.as_bytes().to_vec(), options))
        .await
        .map_err(|_| anyhow::anyhow!("could not authenticate with the Mini over Bluetooth"))?;
    let mut authenticated = false;
    for _ in 0..10 {
        if read(&auth)
            .await
            .map_err(|_| anyhow::anyhow!("could not verify Mini Bluetooth authentication"))?
            == [1]
        {
            authenticated = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !authenticated {
        bail!("Mini rejected the Bluetooth token; read it again in pairing mode");
    }
    Ok(())
}

pub(crate) struct Session {
    events: mpsc::Receiver<anyhow::Result<Event>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Events for Session {
    async fn next(&mut self) -> anyhow::Result<Event> {
        if self.task.is_finished() {
            bail!("Mini Bluetooth connection ended; enter this shot manually");
        }
        self.events
            .recv()
            .await
            .context("Mini Bluetooth event stream ended")?
    }
}

pub(in crate::integration::bluetooth_scale) async fn connect(
    cfg: &MiniConfig,
    adapter: &str,
) -> anyhow::Result<Session> {
    tokio::time::timeout(Duration::from_secs(35), async {
        let gatt = open_device(Connection::system().await?, adapter, &cfg.address).await?;
        subscribe(gatt, &cfg.token).await
    })
    .await
    .context("Mini Bluetooth connection timed out; close other apps using the machine")?
}

pub(in crate::integration::bluetooth_scale) async fn subscribe(
    mut gatt: Gatt,
    token: &str,
) -> anyhow::Result<Session> {
    let shot = characteristic(&gatt, SHOT).await?;
    authenticate(&gatt, token).await?;
    let props = proxy(
        &gatt.bus,
        shot.path().as_str(),
        "org.freedesktop.DBus.Properties",
    )
    .await?;
    let mut signals = props
        .receive_signal_with_args("PropertiesChanged", &[(0, CHAR)])
        .await?;
    gatt.cleanup.notify = Some(shot.clone());
    shot.call::<_, _, ()>("StartNotify", &()).await?;
    let device = proxy(
        &gatt.bus,
        &gatt.device_path,
        "org.freedesktop.DBus.Properties",
    )
    .await?;
    let (tx, events) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        let result: anyhow::Result<()> = async {
            // Keep a slow BlueZ status query out of the notification path.
            // Cancelling/restarting it on every notification could also hide
            // a disconnected device indefinitely, so poll one persistent future.
            let connection = async {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let connected: OwnedValue = tokio::time::timeout(Duration::from_secs(2), device.call("Get", &("org.bluez.Device1", "Connected"))).await??;
                    if !bool::try_from(connected)? { bail!("Mini Bluetooth disconnected; enter this shot manually"); }
                }
            };
            tokio::pin!(connection);
            let mut decoder = Decoder::default();
            let mut last = Instant::now();
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    message = signals.next() => {
                        let message = message.context("Mini Bluetooth notifications ended")?;
                        let (_, mut values, _): (String, HashMap<String, OwnedValue>, Vec<String>) = message.body().deserialize()?;
                        if let Some(value) = values.remove("Value") {
                            let bytes = Vec::<u8>::try_from(value)?;
                            let at = Instant::now();
                            last = at;
                            if let Some(event) = decoder.feed(&bytes, at)? {
                                // Dropping events could splice two shots together. Fail instead.
                                tx.try_send(Ok(event)).map_err(|_| anyhow::anyhow!("Mini event consumer fell behind; enter this shot manually"))?;
                            }
                        }
                    }
                    result = &mut connection => return result,
                    _ = tick.tick() => {
                        if last.elapsed() > Duration::from_secs(60) { bail!("Mini Bluetooth notifications stopped; enter this shot manually"); }
                    }
                }
            }
        }.await;
        // If the consumer is gone/full, closing the channel also fails the shot.
        if let Err(e) = result {
            let _ = tx.try_send(Err(e));
        }
        gatt.cleanup.close().await;
    });
    Ok(Session { events, task })
}
