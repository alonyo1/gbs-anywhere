//! BlueZ peripheral service. A dedicated bus connection owns every exported
//! object, so cancellation or process exit also removes the virtual scale.
use crate::integration::bluetooth_scale::bluez::proxy;
use anyhow::{Context, ensure};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const APP: &str = "/org/gbs/bridge/app";
const SERVICE_PATH: &str = "/org/gbs/bridge/app/service";
const NOTIFY_PATH: &str = "/org/gbs/bridge/app/service/notify";
const WRITE_PATH: &str = "/org/gbs/bridge/app/service/write";
const ADV: &str = "/org/gbs/bridge/advertisement";
const SERVICE: &str = "49535343-fe7d-4ae5-8fa9-9fafd205e455";
const NOTIFY: &str = "49535343-1e4d-4bd9-ba61-23c647249616";
const WRITE: &str = "49535343-8841-43f4-a8d4-ecbe34729bb3";
const CHAR: &str = "org.bluez.GattCharacteristic1";

pub(super) enum Event {
    Subscribed,
    Closed,
    Command(Vec<u8>, Instant),
}
#[derive(Clone)]
struct Shared {
    tx: mpsc::Sender<Event>,
    value: Arc<Mutex<Vec<u8>>>,
    value_at: Arc<Mutex<Option<Instant>>>,
    notifying: Arc<AtomicBool>,
    fault: Arc<AtomicBool>,
    peer_contacted: Arc<AtomicBool>,
    peer: String,
    bluez: String,
}
impl Shared {
    fn sender(&self, h: &zbus::message::Header<'_>) -> zbus::fdo::Result<()> {
        if h.sender().is_none_or(|s| s.as_str() != self.bluez) {
            return Err(zbus::fdo::Error::AccessDenied("BlueZ calls only".into()));
        }
        if self.fault.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("Bridge is unavailable".into()));
        }
        Ok(())
    }
    fn peer(&self, options: &HashMap<String, OwnedValue>) -> zbus::fdo::Result<()> {
        let device = options
            .get("device")
            .and_then(|v| <&zbus::zvariant::ObjectPath<'_>>::try_from(v).ok());
        if device.is_none_or(|p| p.as_str() != self.peer) {
            return Err(zbus::fdo::Error::AccessDenied(
                "Only the selected Mini may use this scale".into(),
            ));
        }
        if options
            .get("offset")
            .is_some_and(|v| u16::try_from(v).unwrap_or(1) != 0)
        {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Offsets are not supported".into(),
            ));
        }
        self.peer_contacted.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn send(&self, e: Event) -> zbus::fdo::Result<()> {
        if self.tx.try_send(e).is_err() {
            self.fault.store(true, Ordering::SeqCst);
            return Err(zbus::fdo::Error::Failed("Bridge command queue full".into()));
        }
        Ok(())
    }
}
struct Service;
#[zbus::interface(name = "org.bluez.GattService1")]
impl Service {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        SERVICE
    }
    #[zbus(property)]
    fn primary(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn includes(&self) -> Vec<OwnedObjectPath> {
        vec![]
    }
}
struct Characteristic {
    shared: Shared,
    write: bool,
}
#[zbus::interface(name = "org.bluez.GattCharacteristic1")]
impl Characteristic {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        if self.write { WRITE } else { NOTIFY }
    }
    #[zbus(property)]
    fn service(&self) -> OwnedObjectPath {
        SERVICE_PATH.try_into().unwrap()
    }
    #[zbus(property)]
    fn flags(&self) -> Vec<&str> {
        if self.write {
            vec!["write", "write-without-response"]
        } else {
            vec!["read", "notify"]
        }
    }
    #[zbus(property)]
    fn value(&self) -> Vec<u8> {
        self.shared.value.lock().unwrap().clone()
    }
    #[zbus(property)]
    fn notifying(&self) -> bool {
        !self.write && self.shared.notifying.load(Ordering::SeqCst)
    }
    fn read_value(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] h: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<Vec<u8>> {
        self.shared.sender(&h)?;
        self.shared.peer(&options)?;
        if self.write {
            return Err(zbus::fdo::Error::NotSupported(
                "Write-only characteristic".into(),
            ));
        }
        if self
            .shared
            .value_at
            .lock()
            .unwrap()
            .is_none_or(|at| at.elapsed() > super::wire::FRESH)
        {
            return Err(zbus::fdo::Error::Failed("No fresh scale value".into()));
        }
        Ok(self.value())
    }
    fn write_value(
        &self,
        value: Vec<u8>,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] h: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        self.shared.sender(&h)?;
        self.shared.peer(&options)?;
        if options
            .get("prepare-authorize")
            .is_some_and(|v| bool::try_from(v).unwrap_or(true))
            || options
                .get("type")
                .is_some_and(|v| !matches!(<&str>::try_from(v), Ok("request" | "command")))
        {
            return Err(zbus::fdo::Error::NotSupported(
                "Prepared writes are not supported".into(),
            ));
        }
        if !self.write
            || !self.shared.notifying.load(Ordering::SeqCst)
            || value.is_empty()
            || value.len() > 256
        {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Invalid scale write or notifications not enabled".into(),
            ));
        }
        self.shared.send(Event::Command(value, Instant::now()))
    }
    fn start_notify(&self, #[zbus(header)] h: zbus::message::Header<'_>) -> zbus::fdo::Result<()> {
        self.shared.sender(&h)?;
        if self.write {
            return Err(zbus::fdo::Error::NotSupported(
                "No notifications on this characteristic".into(),
            ));
        }
        if !self.shared.notifying.swap(true, Ordering::SeqCst) {
            self.shared.send(Event::Subscribed)?;
        }
        Ok(())
    }
    fn stop_notify(&self, #[zbus(header)] h: zbus::message::Header<'_>) -> zbus::fdo::Result<()> {
        self.shared.sender(&h)?;
        if self.shared.notifying.swap(false, Ordering::SeqCst) {
            self.shared.send(Event::Closed)?;
        }
        Ok(())
    }
}
struct Advertisement(String, Shared);
#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    #[zbus(property, name = "Type")]
    fn type_(&self) -> &str {
        "peripheral"
    }
    #[zbus(property, name = "ServiceUUIDs")]
    fn service_uuids(&self) -> Vec<&str> {
        vec![SERVICE]
    }
    #[zbus(property)]
    fn local_name(&self) -> &str {
        &self.0
    }
    fn release(&self, #[zbus(header)] h: zbus::message::Header<'_>) -> zbus::fdo::Result<()> {
        self.1.sender(&h)?;
        self.1.send(Event::Closed)
    }
}

pub(super) struct Peripheral {
    bus: Connection,
    shared: Shared,
    pub events: mpsc::Receiver<Event>,
    manager: Proxy<'static>,
    advertising: Proxy<'static>,
    app_registered: bool,
    advertised: bool,
}
impl Peripheral {
    pub async fn open(adapter: &str, mini: &str) -> anyhow::Result<Self> {
        let bus = Connection::system()
            .await
            .context("cannot reach the system D-Bus")?;
        Self::on_bus(bus, adapter, mini, "LUNAR-C").await
    }
    async fn on_bus(
        bus: Connection,
        adapter: &str,
        mini: &str,
        name: &str,
    ) -> anyhow::Result<Self> {
        let path = format!("/org/bluez/{adapter}");
        let a = proxy(&bus, &path, "org.bluez.Adapter1").await?;
        ensure!(
            a.get_property::<bool>("Powered").await?,
            "Bluetooth adapter is off"
        );
        let roles = a.get_property::<Vec<String>>("Roles").await?;
        ensure!(
            roles.iter().any(|v| v == "central-peripheral"),
            "Enable simultaneous Bluetooth roles on the Linux host before using the bridge; see the bridge setup guide"
        );
        let daemon = zbus::fdo::DBusProxy::new(&bus).await?;
        let bluez = daemon
            .get_name_owner("org.bluez".try_into()?)
            .await?
            .to_string();
        let (tx, events) = mpsc::channel(32);
        let shared = Shared {
            tx,
            value: Arc::new(Mutex::new(Vec::new())),
            value_at: Arc::new(Mutex::new(None)),
            notifying: Arc::new(AtomicBool::new(false)),
            fault: Arc::new(AtomicBool::new(false)),
            peer_contacted: Arc::new(AtomicBool::new(false)),
            peer: format!("{path}/dev_{}", mini.replace(':', "_")),
            bluez,
        };
        let manager = proxy(&bus, &path, "org.bluez.GattManager1").await?;
        let advertising = proxy(&bus, &path, "org.bluez.LEAdvertisingManager1").await?;
        let mut this = Self {
            bus,
            shared,
            events,
            manager,
            advertising,
            app_registered: false,
            advertised: false,
        };
        let server = this.bus.object_server();
        server.at(APP, zbus::fdo::ObjectManager).await?;
        server.at(SERVICE_PATH, Service).await?;
        server
            .at(
                NOTIFY_PATH,
                Characteristic {
                    shared: this.shared.clone(),
                    write: false,
                },
            )
            .await?;
        server
            .at(
                WRITE_PATH,
                Characteristic {
                    shared: this.shared.clone(),
                    write: true,
                },
            )
            .await?;
        server
            .at(ADV, Advertisement(name.into(), this.shared.clone()))
            .await?;
        // Mark attempted registrations before awaiting, so cancellation cleans up.
        this.app_registered = true;
        tokio::time::timeout(
            Duration::from_secs(5),
            this.manager.call::<_, _, ()>(
                "RegisterApplication",
                &(
                    OwnedObjectPath::try_from(APP)?,
                    HashMap::<String, Value<'_>>::new(),
                ),
            ),
        )
        .await
        .context("GATT registration timed out")??;
        Ok(this)
    }
    pub async fn advertise(&mut self) -> anyhow::Result<()> {
        self.advertised = true;
        tokio::time::timeout(
            Duration::from_secs(5),
            self.advertising.call::<_, _, ()>(
                "RegisterAdvertisement",
                &(
                    OwnedObjectPath::try_from(ADV)?,
                    HashMap::<String, Value<'_>>::new(),
                ),
            ),
        )
        .await
        .context("advertising registration timed out")??;
        Ok(())
    }
    pub fn subscribed(&self) -> bool {
        self.shared.notifying.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> anyhow::Result<()> {
        ensure!(
            !self.shared.fault.load(Ordering::SeqCst),
            "bridge command queue overflow; stop the Mini manually"
        );
        Ok(())
    }
    pub async fn notify(&self, value: Vec<u8>) -> anyhow::Result<()> {
        self.check()?;
        ensure!(
            self.subscribed(),
            "Mini disconnected from the virtual scale"
        );
        *self.shared.value.lock().unwrap() = value.clone();
        *self.shared.value_at.lock().unwrap() = Some(Instant::now());
        tokio::time::timeout(
            Duration::from_millis(500),
            self.bus.emit_signal(
                None::<&str>,
                NOTIFY_PATH,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(
                    CHAR,
                    HashMap::from([("Value", Value::from(value))]),
                    Vec::<String>::new(),
                ),
            ),
        )
        .await
        .context("scale notification delivery timed out")??;
        Ok(())
    }
    pub async fn close(&mut self) {
        cleanup(
            self.manager.clone(),
            self.advertising.clone(),
            self.shared.clone(),
            self.app_registered,
            self.advertised,
        )
        .await;
        self.app_registered = false;
        self.advertised = false;
    }
}
async fn cleanup(
    manager: Proxy<'static>,
    advertising: Proxy<'static>,
    shared: Shared,
    app: bool,
    adv: bool,
) {
    shared.fault.store(true, Ordering::SeqCst);
    shared.notifying.store(false, Ordering::SeqCst);
    shared.value.lock().unwrap().clear();
    *shared.value_at.lock().unwrap() = None;
    if adv {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            advertising.call::<_, _, ()>(
                "UnregisterAdvertisement",
                &(OwnedObjectPath::try_from(ADV).unwrap(),),
            ),
        )
        .await;
    }
    if shared.peer_contacted.load(Ordering::SeqCst)
        && let Ok(p) = proxy(manager.connection(), &shared.peer, "org.bluez.Device1").await
    {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            p.call::<_, _, ()>("Disconnect", &()),
        )
        .await;
    }
    if app {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            manager.call::<_, _, ()>(
                "UnregisterApplication",
                &(OwnedObjectPath::try_from(APP).unwrap(),),
            ),
        )
        .await;
    }
}
impl Drop for Peripheral {
    fn drop(&mut self) {
        if !(self.app_registered || self.advertised) {
            return;
        }
        let future = cleanup(
            self.manager.clone(),
            self.advertising.clone(),
            self.shared.clone(),
            self.app_registered,
            self.advertised,
        );
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(future);
        }
    }
}

#[cfg(test)]
#[path = "gatt_tests.rs"]
mod tests;
