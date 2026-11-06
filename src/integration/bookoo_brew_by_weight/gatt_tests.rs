use super::*;
use std::io::BufRead;
struct Bus(std::process::Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn private_bus() -> (Bus, String) {
    let mut b = Bus(std::process::Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap());
    let mut a = String::new();
    std::io::BufReader::new(b.0.stdout.take().unwrap())
        .read_line(&mut a)
        .unwrap();
    (b, a.trim().into())
}
struct Adapter;
#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn powered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn roles(&self) -> Vec<&str> {
        vec!["central", "peripheral", "central-peripheral"]
    }
}
struct Manager(Arc<AtomicBool>);
#[zbus::interface(name = "org.bluez.GattManager1")]
impl Manager {
    fn register_application(&self, path: OwnedObjectPath, _options: HashMap<String, OwnedValue>) {
        assert_eq!(path.as_str(), APP);
        self.0.store(true, Ordering::SeqCst);
    }
    fn unregister_application(&self, _path: OwnedObjectPath) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct Advertising(Arc<AtomicBool>);
#[zbus::interface(name = "org.bluez.LEAdvertisingManager1")]
impl Advertising {
    fn register_advertisement(&self, path: OwnedObjectPath, _options: HashMap<String, OwnedValue>) {
        assert_eq!(path.as_str(), ADV);
        self.0.store(true, Ordering::SeqCst);
    }
    fn unregister_advertisement(&self, _path: OwnedObjectPath) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct Device(Arc<AtomicBool>);
#[zbus::interface(name = "org.bluez.Device1")]
impl Device {
    fn disconnect(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn exports_real_gatt_rejects_other_peers_and_cleans_up() {
    let (_daemon, address) = private_bus();
    let app = Arc::new(AtomicBool::new(false));
    let adv = Arc::new(AtomicBool::new(false));
    let disconnected = Arc::new(AtomicBool::new(false));
    let peer = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";
    let bluez = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .name("org.bluez")
        .unwrap()
        .serve_at("/org/bluez/hci0", Adapter)
        .unwrap()
        .serve_at("/org/bluez/hci0", Manager(app.clone()))
        .unwrap()
        .serve_at("/org/bluez/hci0", Advertising(adv.clone()))
        .unwrap()
        .serve_at(peer, Device(disconnected.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
    let bus = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let destination = bus.unique_name().unwrap().to_string();
    let mut p = Peripheral::on_bus(bus, "hci0", "AA:BB:CC:DD:EE:FF", "LUNAR-C")
        .await
        .unwrap();
    p.advertise().await.unwrap();
    assert!(app.load(Ordering::SeqCst) && adv.load(Ordering::SeqCst));
    let manager = zbus::fdo::ObjectManagerProxy::builder(&bluez)
        .destination(destination.as_str())
        .unwrap()
        .path(APP)
        .unwrap()
        .build()
        .await
        .unwrap();
    let objects = manager.get_managed_objects().await.unwrap();
    assert_eq!(objects.len(), 3);
    let c = Proxy::new(&bluez, destination.as_str(), NOTIFY_PATH, CHAR)
        .await
        .unwrap();
    c.call::<_, _, ()>("StartNotify", &()).await.unwrap();
    assert!(matches!(p.events.recv().await, Some(Event::Subscribed)));
    let w = Proxy::new(&bluez, destination.as_str(), WRITE_PATH, CHAR)
        .await
        .unwrap();
    let opts = |path: &str| {
        HashMap::from([(
            "device",
            Value::from(OwnedObjectPath::try_from(path).unwrap()),
        )])
    };
    let hello = super::super::wire::packet(11, &[0; 15]);
    assert!(
        w.call::<_, _, ()>(
            "WriteValue",
            &(hello.clone(), opts("/org/bluez/hci0/dev_11_22_33_44_55_66"))
        )
        .await
        .is_err()
    );
    assert!(p.events.try_recv().is_err());
    let mut prepared = opts(peer);
    prepared.insert("prepare-authorize", Value::from(true));
    assert!(
        w.call::<_, _, ()>("WriteValue", &(hello.clone(), prepared))
            .await
            .is_err()
    );
    assert!(p.events.try_recv().is_err());
    assert!(
        c.call::<_, _, Vec<u8>>("ReadValue", &(opts(peer),))
            .await
            .is_err()
    );
    w.call::<_, _, ()>("WriteValue", &(hello.clone(), opts(peer)))
        .await
        .unwrap();
    assert!(matches!(p.events.recv().await,Some(Event::Command(b,_)) if b==hello));
    let properties = Proxy::new(
        &bluez,
        destination.as_str(),
        NOTIFY_PATH,
        "org.freedesktop.DBus.Properties",
    )
    .await
    .unwrap();
    use futures_util::StreamExt;
    let mut signals = properties
        .receive_signal("PropertiesChanged")
        .await
        .unwrap();
    p.notify(vec![1, 2, 3]).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(1), signals.next())
        .await
        .unwrap()
        .unwrap();
    let (_, values, _): (String, HashMap<String, OwnedValue>, Vec<String>) =
        msg.body().deserialize().unwrap();
    assert_eq!(
        Vec::<u8>::try_from(values.get("Value").unwrap().try_clone().unwrap()).unwrap(),
        [1, 2, 3]
    );
    assert_eq!(
        c.call::<_, _, Vec<u8>>("ReadValue", &(opts(peer),))
            .await
            .unwrap(),
        [1, 2, 3]
    );
    *p.shared.value_at.lock().unwrap() = Some(Instant::now() - Duration::from_secs(2));
    assert!(
        c.call::<_, _, Vec<u8>>("ReadValue", &(opts(peer),))
            .await
            .is_err()
    );
    let stranger = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let unauthorized = Proxy::new(&stranger, destination.as_str(), WRITE_PATH, CHAR)
        .await
        .unwrap();
    assert!(
        unauthorized
            .call::<_, _, ()>("WriteValue", &(hello, opts(peer)))
            .await
            .is_err()
    );
    p.close().await;
    assert!(!app.load(Ordering::SeqCst) && !adv.load(Ordering::SeqCst));
    assert!(disconnected.load(Ordering::SeqCst));
}
#[tokio::test]
#[ignore = "Explicit host smoke test: briefly advertises GBS-PROBE, without scale data or machine commands"]
async fn host_gatt_smoke() {
    let bus = Connection::system().await.unwrap();
    let mut p = Peripheral::on_bus(bus, "hci0", "AA:BB:CC:DD:EE:FF", "GBS-PROBE")
        .await
        .unwrap();
    p.advertise().await.unwrap();
    p.close().await;
}
