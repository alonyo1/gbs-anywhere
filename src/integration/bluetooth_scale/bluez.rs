//! Linux GATT client using the host's BlueZ service. No raw HCI access.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use futures_util::{FutureExt, StreamExt};
use zbus::fdo::{ManagedObjects, ObjectManagerProxy};
use zbus::proxy::SignalStream;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, Proxy};

use super::config::Config;
use super::run::Scale;
use super::{Decoder, Model};
use crate::integration::scale_shot::Reading;

const DEVICE: &str = "org.bluez.Device1";
const SERVICE: &str = "org.bluez.GattService1";
const CHARACTERISTIC: &str = "org.bluez.GattCharacteristic1";

pub(crate) async fn discover(adapter: &str) -> anyhow::Result<Vec<super::DiscoveredScale>> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let bus = Connection::system()
            .await
            .context("cannot reach the system D-Bus")?;
        discover_on_bus(&bus, adapter, Duration::from_secs(8)).await
    })
    .await
    .context("Bluetooth discovery timed out")?
}

async fn discover_on_bus(
    bus: &Connection,
    adapter: &str,
    duration: Duration,
) -> anyhow::Result<Vec<super::DiscoveredScale>> {
    let path = format!("/org/bluez/{adapter}");
    let adapter = proxy(bus, &path, "org.bluez.Adapter1").await?;
    if !adapter.get_property::<bool>("Powered").await? {
        bail!("Bluetooth adapter is off; power it on on the host");
    }
    let filter: HashMap<&str, Value<'_>> = HashMap::from([("Transport", Value::from("le"))]);
    adapter
        .call::<_, _, ()>("SetDiscoveryFilter", &(filter,))
        .await?;
    let mut cleanup = Cleanup {
        discovery: Some(adapter.clone()),
        notify: None,
        device: None,
    };
    adapter
        .call::<_, _, ()>("StartDiscovery", &())
        .await
        .context("cannot scan for Bluetooth devices")?;
    tokio::time::sleep(duration).await;
    let manager = ObjectManagerProxy::builder(bus)
        .destination("org.bluez")?
        .path("/")?
        .build()
        .await?;
    let devices = discovered(&manager.get_managed_objects().await?, &path);
    cleanup.close().await;
    Ok(devices)
}

/// BlueZ can include cached devices. Keep the result bounded, prefer named
/// devices and a stronger signal, and never infer compatibility from a name.
fn discovered(objects: &ManagedObjects, adapter: &str) -> Vec<super::DiscoveredScale> {
    let mut devices: Vec<_> = objects
        .iter()
        .filter_map(|(path, interfaces)| {
            if !path.as_str().starts_with(&format!("{adapter}/dev_")) {
                return None;
            }
            let props = interfaces.get(DEVICE)?;
            let address = <&str>::try_from(props.get("Address")?).ok()?.to_owned();
            let name = props
                .get("Name")
                .and_then(|v| <&str>::try_from(v).ok())
                .map(|s| {
                    s.chars()
                        .take(128)
                        .filter(|c| !c.is_control())
                        .collect::<String>()
                })
                .filter(|s| !s.is_empty());
            let rssi_dbm = props.get("RSSI").and_then(|v| i16::try_from(v).ok());
            let connected = props
                .get("Connected")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false);
            Some(super::DiscoveredScale {
                address,
                name,
                rssi_dbm,
                connected,
            })
        })
        .collect();
    devices.sort_by(|a, b| {
        b.name
            .is_some()
            .cmp(&a.name.is_some())
            .then_with(|| b.rssi_dbm.cmp(&a.rssi_dbm))
            .then_with(|| a.address.cmp(&b.address))
    });
    devices.truncate(64);
    devices
}

pub(crate) async fn proxy(
    bus: &Connection,
    path: &str,
    interface: &'static str,
) -> zbus::Result<Proxy<'static>> {
    Proxy::new_owned(bus.clone(), "org.bluez", path.to_owned(), interface).await
}

/// Calls are bounded even when BlueZ or the radio stops answering.
pub(crate) async fn connect(cfg: &Config, model: Model) -> anyhow::Result<Session> {
    tokio::time::timeout(Duration::from_secs(35), open(cfg, model))
        .await
        .context("Bluetooth connection timed out; turn on the scale and close other scale apps")?
}

/// Releases only discovery, notification and device sessions this client owns,
/// including when its integration task is cancelled during a connection.
#[derive(Default)]
pub(crate) struct Cleanup {
    discovery: Option<Proxy<'static>>,
    pub(crate) notify: Option<Proxy<'static>>,
    device: Option<Proxy<'static>>,
}

impl Cleanup {
    pub(crate) async fn close(&mut self) {
        close_parts(
            self.discovery.take(),
            self.notify.take(),
            self.device.take(),
        )
        .await;
    }
}

async fn close_parts(
    discovery: Option<Proxy<'static>>,
    notify: Option<Proxy<'static>>,
    device: Option<Proxy<'static>>,
) {
    for (proxy, method) in [
        (discovery, "StopDiscovery"),
        (notify, "StopNotify"),
        (device, "Disconnect"),
    ] {
        if let Some(proxy) = proxy {
            let _ =
                tokio::time::timeout(Duration::from_secs(2), proxy.call::<_, _, ()>(method, &()))
                    .await;
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.discovery.is_none() && self.notify.is_none() && self.device.is_none() {
            return;
        }
        // The spawned future must not own another Cleanup: runtime shutdown
        // can cancel it, and recursively spawning from Drop would overflow.
        let future = close_parts(
            self.discovery.take(),
            self.notify.take(),
            self.device.take(),
        );
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(future);
        }
    }
}

async fn open(cfg: &Config, model: Model) -> anyhow::Result<Session> {
    let bus = Connection::system()
        .await
        .context("cannot reach the system D-Bus; Docker needs /run/dbus mounted read-only")?;
    open_on_bus(bus, cfg, model).await
}

async fn open_on_bus(bus: Connection, cfg: &Config, model: Model) -> anyhow::Result<Session> {
    let Gatt {
        bus,
        device_path,
        objects,
        mut cleanup,
        ..
    } = open_device(bus, &cfg.adapter, &cfg.address).await?;
    let (weight_path, command_path) = characteristics(&objects, &device_path, model).context(
        "selected device does not provide the selected scale service and characteristics",
    )?;
    let weight = proxy(&bus, &weight_path, CHARACTERISTIC).await?;
    let command = proxy(&bus, &command_path, CHARACTERISTIC).await?;
    let flags = command.get_property::<Vec<String>>("Flags").await?;
    let write_type = if flags.iter().any(|s| s == "write") {
        "request"
    } else if flags.iter().any(|s| s == "write-without-response") {
        "command"
    } else {
        bail!("the scale's command characteristic is not writable")
    };
    let properties = proxy(&bus, &weight_path, "org.freedesktop.DBus.Properties").await?;
    // Subscribe before enabling notifications, so the first reading is kept.
    let signals = properties
        .receive_signal_with_args("PropertiesChanged", &[(0, CHARACTERISTIC)])
        .await?;
    cleanup.notify = Some(weight.clone());
    weight
        .call::<_, _, ()>("StartNotify", &())
        .await
        .context("could not enable scale readings")?;
    Ok(Session {
        model,
        decoder: Decoder::new(model),
        pending: VecDeque::new(),
        signals,
        command,
        write_type,
        clock: Instant::now(),
        _cleanup: cleanup,
    })
}

pub(crate) struct Gatt {
    pub(crate) bus: Connection,
    pub(crate) device_path: String,
    pub(crate) objects: ManagedObjects,
    pub(crate) cleanup: Cleanup,
}

pub(crate) async fn open_device(
    bus: Connection,
    adapter_name: &str,
    address: &str,
) -> anyhow::Result<Gatt> {
    let adapter_path = format!("/org/bluez/{}", adapter_name);
    let adapter = proxy(&bus, &adapter_path, "org.bluez.Adapter1").await?;
    if !adapter.get_property::<bool>("Powered").await.context(
        "cannot access the Bluetooth adapter; install/start BlueZ and check D-Bus permissions",
    )? {
        bail!("Bluetooth adapter is off; power it on with bluetoothctl on the host");
    }
    let manager = ObjectManagerProxy::builder(&bus)
        .destination("org.bluez")?
        .path("/")?
        .build()
        .await?;
    let device_path = format!("{adapter_path}/dev_{}", address.replace(':', "_"));
    let mut cleanup = Cleanup::default();
    let mut objects = manager.get_managed_objects().await?;
    if !has_device(&objects, &device_path) {
        // Some firmware omits service UUIDs from advertisements. Discover
        // LE devices, then match only the configured address and GATT service.
        let filter: HashMap<&str, Value<'_>> = HashMap::from([("Transport", Value::from("le"))]);
        adapter
            .call::<_, _, ()>("SetDiscoveryFilter", &(filter,))
            .await?;
        cleanup.discovery = Some(adapter.clone());
        adapter
            .call::<_, _, ()>("StartDiscovery", &())
            .await
            .context("cannot scan for the Bluetooth device; check BlueZ D-Bus permissions")?;
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            objects = manager.get_managed_objects().await?;
            if has_device(&objects, &device_path) {
                break;
            }
        }
        adapter.call::<_, _, ()>("StopDiscovery", &()).await?;
        cleanup.discovery = None;
    }
    let device = proxy(&bus, &device_path, DEVICE).await?;
    if !device.get_property::<bool>("Connected").await? {
        cleanup.device = Some(device.clone());
        device
            .call::<_, _, ()>("Connect", &())
            .await
            .context("could not connect to the selected device; close phone apps using it")?;
    }
    while !device.get_property::<bool>("ServicesResolved").await? {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let objects = manager.get_managed_objects().await?;
    Ok(Gatt {
        bus,
        device_path,
        objects,
        cleanup,
    })
}

fn has_device(objects: &ManagedObjects, path: &str) -> bool {
    objects
        .iter()
        .any(|(p, interfaces)| p.as_str() == path && interfaces.contains_key(DEVICE))
}

/// Both characteristics must belong to the scale's service on the selected
/// device, not to a similarly named service or another nearby scale.
fn characteristics(
    objects: &ManagedObjects,
    device: &str,
    model: Model,
) -> Option<(String, String)> {
    let prefix = format!("{device}/");
    let service = objects.iter().find_map(|(path, interfaces)| {
        let props = interfaces.get(SERVICE)?;
        let uuid = <&str>::try_from(props.get("UUID")?).ok()?;
        (path.as_str().starts_with(&prefix) && uuid.eq_ignore_ascii_case(model.service()))
            .then_some(path)
    })?;
    let mut weight = None;
    let mut command = None;
    for (path, interfaces) in objects {
        let Some(props) = interfaces.get(CHARACTERISTIC) else {
            continue;
        };
        let Some(parent) = props
            .get("Service")
            .and_then(|v| <&zbus::zvariant::ObjectPath<'_>>::try_from(v).ok())
        else {
            continue;
        };
        if parent.as_str() != service.as_str() {
            continue;
        }
        let Some(uuid) = props.get("UUID").and_then(|v| <&str>::try_from(v).ok()) else {
            continue;
        };
        if uuid.eq_ignore_ascii_case(model.weight()) {
            weight = Some(path.to_string());
        }
        if uuid.eq_ignore_ascii_case(model.command()) {
            command = Some(path.to_string());
        }
    }
    Some((weight?, command?))
}

pub(crate) struct Session {
    model: Model,
    decoder: Decoder,
    pending: VecDeque<f64>,
    signals: SignalStream<'static>,
    command: Proxy<'static>,
    write_type: &'static str,
    clock: Instant,
    _cleanup: Cleanup,
}

pub(crate) struct Writer {
    command: Proxy<'static>,
    write_type: &'static str,
}
impl Writer {
    pub(crate) async fn write(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        let options: HashMap<&str, Value<'_>> =
            HashMap::from([("type", Value::from(self.write_type))]);
        tokio::time::timeout(
            Duration::from_secs(2),
            self.command
                .call::<_, _, ()>("WriteValue", &(bytes, options)),
        )
        .await
        .context("Bluetooth scale command timed out")??;
        Ok(())
    }
}
impl Session {
    pub(crate) fn writer(&self) -> Writer {
        Writer {
            command: self.command.clone(),
            write_type: self.write_type,
        }
    }
    pub(crate) async fn packet(&mut self) -> anyhow::Result<Vec<u8>> {
        loop {
            let Some(message) = self.signals.next().await else {
                return Err(anyhow!("Bluetooth notifications ended"));
            };
            let (_, mut changed, _): (String, HashMap<String, OwnedValue>, Vec<String>) = message
                .body()
                .deserialize()
                .context("invalid Bluetooth property notification")?;
            let Some(value) = changed.remove("Value") else {
                continue;
            };
            let Ok(packet) = Vec::<u8>::try_from(value) else {
                continue;
            };
            return Ok(packet);
        }
    }

    pub(crate) async fn close(mut self) {
        self._cleanup.close().await;
    }
}

impl Scale for Session {
    async fn next(&mut self) -> anyhow::Result<Reading> {
        loop {
            if let Some(grams) = self.pending.pop_front() {
                return Ok(Reading {
                    ms: self.clock.elapsed().as_millis() as u64,
                    grams,
                });
            }
            let packet = self.packet().await?;
            self.pending.extend(self.decoder.weights(&packet)?);
        }
    }

    async fn tare(&mut self) -> anyhow::Result<()> {
        self.pending.clear();
        while self.signals.next().now_or_never().flatten().is_some() {}
        self.write(self.model.tare()).await
    }
}

impl Session {
    pub(crate) async fn write(&mut self, bytes: Vec<u8>) -> anyhow::Result<()> {
        let options: HashMap<&str, Value<'_>> =
            HashMap::from([("type", Value::from(self.write_type))]);
        tokio::time::timeout(
            Duration::from_secs(2),
            self.command
                .call::<_, _, ()>("WriteValue", &(bytes, options)),
        )
        .await
        .context("Bluetooth scale command timed out")??;
        Ok(())
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::bookoo_bluetooth::protocol;
    use zbus::zvariant::OwnedObjectPath;

    #[test]
    fn characteristics_are_bound_to_the_selected_device_and_service() {
        let device = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";
        let service = format!("{device}/service001");
        let mut objects = ManagedObjects::new();
        let uuid = |s: &str| OwnedValue::from(zbus::zvariant::Str::from(s.to_owned()));
        objects.insert(
            OwnedObjectPath::try_from(service.clone()).unwrap(),
            HashMap::from([(
                SERVICE.try_into().unwrap(),
                HashMap::from([("UUID".into(), uuid(protocol::SERVICE))]),
            )]),
        );
        for (n, id) in [(1, protocol::WEIGHT), (2, protocol::COMMAND)] {
            objects.insert(
                OwnedObjectPath::try_from(format!("{service}/char{n}")).unwrap(),
                HashMap::from([(
                    CHARACTERISTIC.try_into().unwrap(),
                    HashMap::from([
                        ("UUID".into(), uuid(id)),
                        (
                            "Service".into(),
                            Value::from(OwnedObjectPath::try_from(service.clone()).unwrap())
                                .try_into()
                                .unwrap(),
                        ),
                    ]),
                )]),
            );
        }
        assert!(characteristics(&objects, device, Model::Bookoo).is_some());
        assert!(
            characteristics(
                &objects,
                "/org/bluez/hci1/dev_AA_BB_CC_DD_EE_FF",
                Model::Bookoo
            )
            .is_none()
        );
        objects.remove(&OwnedObjectPath::try_from(format!("{service}/char2")).unwrap());
        assert!(characteristics(&objects, device, Model::Bookoo).is_none());
    }
}
