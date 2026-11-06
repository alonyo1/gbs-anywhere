//! Exercise the real GATT client on a private D-Bus, without a radio or scale.

use super::*;
use crate::integration::bookoo_bluetooth::protocol;
use std::io::BufRead;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use zbus::zvariant::OwnedObjectPath;

struct PrivateBus(std::process::Child);

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn private_bus() -> (PrivateBus, String) {
    let mut daemon = PrivateBus(
        std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("install dbus-daemon to run the Bluetooth transport test"),
    );
    let mut address = String::new();
    std::io::BufReader::new(daemon.0.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    (daemon, address.trim().to_owned())
}

#[test]
fn pending_cleanup_can_be_cancelled_when_the_runtime_stops() {
    let (_daemon, address) = private_bus();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let bus = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let proxy = Proxy::new_owned(
            bus,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        .unwrap();
        drop(Cleanup {
            device: Some(proxy),
            discovery: None,
            notify: None,
        });
        // Return before the queued cleanup runs. Dropping the runtime must
        // cancel it without recursively scheduling another cleanup or panicking.
    });
    drop(runtime);
}

struct Adapter(Arc<AtomicBool>);
#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn powered(&self) -> bool {
        true
    }
    fn set_discovery_filter(&self, filter: HashMap<String, OwnedValue>) {
        assert_eq!(
            <&str>::try_from(filter.get("Transport").unwrap()).unwrap(),
            "le"
        );
    }
    fn start_discovery(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn stop_discovery(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

struct Device(Arc<AtomicBool>, Duration, Arc<AtomicBool>);
#[zbus::interface(name = "org.bluez.Device1")]
impl Device {
    #[zbus(property)]
    fn address(&self) -> &str {
        "AA:BB:CC:DD:EE:FF"
    }
    #[zbus(property)]
    fn name(&self) -> &str {
        "Demo scale"
    }
    #[zbus(property, name = "RSSI")]
    fn rssi(&self) -> i16 {
        -42
    }
    #[zbus(property)]
    async fn connected(&self) -> bool {
        self.2.store(true, Ordering::SeqCst);
        tokio::time::sleep(self.1).await;
        self.0.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn services_resolved(&self) -> bool {
        true
    }
    fn connect(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn disconnect(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

struct Service(&'static str);
#[zbus::interface(name = "org.bluez.GattService1")]
impl Service {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        self.0
    }
}

struct Characteristic {
    uuid: &'static str,
    service: OwnedObjectPath,
    notifying: Arc<AtomicBool>,
    written: Arc<Mutex<Vec<u8>>>,
}
#[zbus::interface(name = "org.bluez.GattCharacteristic1")]
impl Characteristic {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        self.uuid
    }
    #[zbus(property)]
    fn service(&self) -> OwnedObjectPath {
        self.service.clone()
    }
    #[zbus(property)]
    fn flags(&self) -> Vec<&str> {
        vec!["notify", "write"]
    }
    fn read_value(&self, _options: HashMap<String, OwnedValue>) -> Vec<u8> {
        if self.uuid.starts_with("0d0b") {
            vec![u8::from(
                *self.written.lock().unwrap() == b"sample-local-token",
            )]
        } else {
            b"sample-local-token".to_vec()
        }
    }
    fn start_notify(&self) {
        self.notifying.store(true, Ordering::SeqCst);
    }
    fn stop_notify(&self) {
        self.notifying.store(false, Ordering::SeqCst);
    }
    fn write_value(&self, bytes: Vec<u8>, options: HashMap<String, OwnedValue>) {
        assert_eq!(
            <&str>::try_from(options.get("type").unwrap()).unwrap(),
            "request"
        );
        *self.written.lock().unwrap() = bytes;
    }
}

#[tokio::test]
async fn reads_and_tares_through_a_private_bluez_service() {
    exercise(Model::Bookoo).await;
}

#[tokio::test]
async fn discovery_lists_devices_without_connecting_and_cleans_up_on_cancel() {
    let (_daemon, address) = private_bus();
    let scanning = Arc::new(AtomicBool::new(false));
    let connected = Arc::new(AtomicBool::new(false));
    let _server = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .name("org.bluez")
        .unwrap()
        .serve_at("/", zbus::fdo::ObjectManager)
        .unwrap()
        .serve_at("/org/bluez/hci0", Adapter(scanning.clone()))
        .unwrap()
        .serve_at(
            "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
            Device(connected.clone(), Duration::ZERO, Arc::default()),
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let bus = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let devices = discover_on_bus(&bus, "hci0", Duration::from_millis(10))
        .await
        .unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].address, "AA:BB:CC:DD:EE:FF");
    assert_eq!(devices[0].name.as_deref(), Some("Demo scale"));
    assert_eq!(devices[0].rssi_dbm, Some(-42));
    assert!(!scanning.load(Ordering::SeqCst));
    assert!(!connected.load(Ordering::SeqCst));
    let scan =
        tokio::spawn(async move { discover_on_bus(&bus, "hci0", Duration::from_secs(60)).await });
    crate::server::wait_for("scan started", || scanning.load(Ordering::SeqCst)).await;
    scan.abort();
    assert!(scan.await.unwrap_err().is_cancelled());
    crate::server::wait_for("scan stopped", || !scanning.load(Ordering::SeqCst)).await;
    assert!(!connected.load(Ordering::SeqCst));
}

async fn exercise(model: Model) {
    let (_daemon, address) = private_bus();
    let address = address.as_str();
    let device = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";
    let service = format!("{device}/service001");
    let weight = format!("{service}/char001");
    let command = format!("{service}/char002");
    let connected = Arc::new(AtomicBool::new(false));
    let notifying = Arc::new(AtomicBool::new(false));
    let written = Arc::new(Mutex::new(Vec::new()));
    let characteristic = |uuid| Characteristic {
        uuid,
        service: OwnedObjectPath::try_from(service.clone()).unwrap(),
        notifying: notifying.clone(),
        written: written.clone(),
    };
    let server = zbus::connection::Builder::address(address)
        .unwrap()
        .name("org.bluez")
        .unwrap()
        .serve_at("/", zbus::fdo::ObjectManager)
        .unwrap()
        .serve_at("/org/bluez/hci0", Adapter(Arc::default()))
        .unwrap()
        .serve_at(
            device,
            Device(connected.clone(), Duration::ZERO, Arc::default()),
        )
        .unwrap()
        .serve_at(service.as_str(), Service(model.service()))
        .unwrap()
        .serve_at(weight.as_str(), characteristic(model.weight()))
        .unwrap()
        .serve_at(command.as_str(), characteristic(model.command()))
        .unwrap()
        .build()
        .await
        .unwrap();
    let bus = zbus::connection::Builder::address(address)
        .unwrap()
        .build()
        .await
        .unwrap();
    let mut scale = open_on_bus(
        bus,
        &Config {
            address: "AA:BB:CC:DD:EE:FF".into(),
            adapter: "hci0".into(),
            mini: None,
        },
        model,
    )
    .await
    .unwrap();
    assert!(connected.load(Ordering::SeqCst));
    assert!(notifying.load(Ordering::SeqCst));
    let frame = protocol::frame(38.4);
    let values: HashMap<&str, Value<'_>> = HashMap::from([("Value", Value::from(frame))]);
    server
        .emit_signal(
            None::<&str>,
            weight.as_str(),
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(CHARACTERISTIC, values, Vec::<String>::new()),
        )
        .await
        .unwrap();
    let r = tokio::time::timeout(Duration::from_secs(2), scale.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.grams, 38.4);
    scale.tare().await.unwrap();
    assert_eq!(*written.lock().unwrap(), model.tare());
    drop(scale);
    crate::server::wait_for("Bluetooth cleanup", || {
        !notifying.load(Ordering::SeqCst) && !connected.load(Ordering::SeqCst)
    })
    .await;
}

#[tokio::test]
async fn mini_authenticates_reads_events_and_releases_its_connection() {
    use crate::integration::bluetooth_scale::mini::{self, Event, Events};
    let (_daemon, address) = private_bus();
    let device = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";
    let service = format!("{device}/service001");
    let auth = format!("{service}/char001");
    let shot = format!("{service}/char002");
    let connected = Arc::new(AtomicBool::new(false));
    let notifying = Arc::new(AtomicBool::new(false));
    let written = Arc::new(Mutex::new(Vec::new()));
    let checking = Arc::new(AtomicBool::new(false));
    let characteristic = |uuid| Characteristic {
        uuid,
        service: OwnedObjectPath::try_from(service.clone()).unwrap(),
        notifying: notifying.clone(),
        written: written.clone(),
    };
    let server = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .name("org.bluez")
        .unwrap()
        .serve_at("/", zbus::fdo::ObjectManager)
        .unwrap()
        .serve_at("/org/bluez/hci0", Adapter(Arc::default()))
        .unwrap()
        .serve_at(
            device,
            Device(connected.clone(), Duration::from_secs(1), checking.clone()),
        )
        .unwrap()
        .serve_at(service.as_str(), Service("mini-service"))
        .unwrap()
        .serve_at(
            auth.as_str(),
            characteristic("0d0b7847-e12b-09a8-b04b-8e0922a9abab"),
        )
        .unwrap()
        .serve_at(
            shot.as_str(),
            characteristic("0e0b7847-e12b-09a8-b04b-8e0922a9abab"),
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = || {
        zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
    };
    let gatt = open_device(client().await.unwrap(), "hci0", "AA:BB:CC:DD:EE:FF")
        .await
        .unwrap();
    let err = mini::bluez::subscribe(gatt, "wrong-private-token")
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(!err.contains("wrong-private-token"));
    crate::server::wait_for("failed authentication cleanup", || {
        !connected.load(Ordering::SeqCst)
    })
    .await;
    let gatt = open_device(client().await.unwrap(), "hci0", "AA:BB:CC:DD:EE:FF")
        .await
        .unwrap();
    let mut mini = mini::bluez::subscribe(gatt, "sample-local-token")
        .await
        .unwrap();
    checking.store(false, Ordering::SeqCst);
    assert_eq!(*written.lock().unwrap(), b"sample-local-token");
    assert!(notifying.load(Ordering::SeqCst));
    // A one-second status query must not block arriving brew events.
    crate::server::wait_for("slow Mini connection check", || {
        checking.load(Ordering::SeqCst)
    })
    .await;
    // First packet replays an old start; telemetry must not revive it.
    for value in [
        serde_json::json!({"BrewingStartedGroup1DoseIndex":"Continuous"}),
        serde_json::json!({"CoffeeBoiler1UpdateTemperature":93}),
        serde_json::json!({"BrewingStartedGroup1DoseIndex":"Continuous"}),
        serde_json::json!({"BrewingStoppedGroup1Time":28.5}),
    ] {
        let values = HashMap::from([("Value", Value::from(serde_json::to_vec(&value).unwrap()))]);
        server
            .emit_signal(
                None::<&str>,
                shot.as_str(),
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(CHARACTERISTIC, values, Vec::<String>::new()),
            )
            .await
            .unwrap();
    }
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(500), mini.next())
            .await
            .unwrap()
            .unwrap(),
        Event::Started { .. }
    ));
    assert!(
        matches!(tokio::time::timeout(Duration::from_millis(500), mini.next()).await.unwrap().unwrap(), Event::Stopped { time: Some(t), .. } if t == Duration::from_millis(28500))
    );
    // No replayed start or telemetry event was enqueued.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), mini.next())
            .await
            .is_err()
    );
    connected.store(false, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), mini.next())
            .await
            .unwrap()
            .is_err()
    );
    drop(mini);
    crate::server::wait_for("Mini notification cleanup", || {
        !notifying.load(Ordering::SeqCst)
    })
    .await;
}
