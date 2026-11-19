//! The server on a real bus: a `dbus-daemon` of the test's own, an application raising and closing
//! notifications through it, and a second server waiting for the name. Skipped where no `dbus-daemon` is
//! installed.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use notifications::backend::{Bus, Said, Server};
use notifications::store::{Reason, Store};
use tokio::sync::mpsc;
use zbus::{proxy, Connection};
use zvariant::Value;

#[proxy(interface = "org.freedesktop.Notifications", default_service = "org.freedesktop.Notifications", default_path = "/org/freedesktop/Notifications")]
trait Notifications {
    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;
    #[allow(clippy::too_many_arguments)]
    fn notify(&self, app_name: &str, replaces_id: u32, app_icon: &str, summary: &str, body: &str, actions: &[&str], hints: HashMap<&str, &Value<'_>>, expire_timeout: i32) -> zbus::Result<u32>;
    fn close_notification(&self, id: u32) -> zbus::Result<()>;
    fn get_server_information(&self) -> zbus::Result<(String, String, String, String)>;
    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
}

struct Daemon(Child, String);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn daemon() -> Option<Daemon> {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let mut address = String::new();
    let mut byte = [0u8; 1];
    use std::io::Read;
    while out.read_exact(&mut byte).is_ok() && byte[0] != b'\n' {
        address.push(byte[0] as char);
    }
    Some(Daemon(child, address))
}

async fn next(said: &mut mpsc::UnboundedReceiver<Said>) -> Said {
    tokio::time::timeout(Duration::from_secs(5), said.recv()).await.expect("the server says something").unwrap()
}

#[tokio::test]
async fn an_application_raises_closes_and_hears_through_the_bus() {
    let Some(d) = daemon() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    // The one variable a unit may be handed; the test's own bus.
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &d.1);

    let store = Arc::new(Mutex::new(Store::new(10)));
    let (tx, mut said) = mpsc::unbounded_channel();
    let server = Server::start(store.clone(), tx, Duration::from_millis(200)).await.expect("the server serves");
    assert!(matches!(next(&mut said).await, Said::Held { held: true, .. }));

    let app = Connection::session().await.unwrap();
    let proxy = NotificationsProxy::new(&app).await.unwrap();
    let mut closed = proxy.receive_notification_closed().await.unwrap();
    let mut invoked = proxy.receive_action_invoked().await.unwrap();
    assert_eq!(proxy.get_server_information().await.unwrap().3, "1.2");
    assert!(proxy.get_capabilities().await.unwrap().contains(&"actions".to_string()));

    let critical = Value::U8(2);
    let entry = Value::from("org.mozilla.Thunderbird");
    let hints = HashMap::from([("urgency", &critical), ("desktop-entry", &entry)]);
    let id = proxy.notify("Thunderbird", 0, "mail", "Dinner", "Lunch tomorrow?", &["default", "Open"], hints, -1).await.unwrap();
    assert_eq!(next(&mut said).await, Said::Raised { id, replaced: false });
    {
        let s = store.lock().unwrap();
        let n = s.get(id).unwrap();
        assert_eq!(n.incoming.urgency.as_str(), "critical");
        assert_eq!(n.incoming.desktop_entry.as_deref(), Some("org.mozilla.Thunderbird"));
        assert_eq!(n.incoming.actions, vec![("default".to_string(), "Open".to_string())]);
    }

    // The application closes its own.
    proxy.close_notification(id).await.unwrap();
    assert_eq!(next(&mut said).await, Said::Changed);
    let c = closed.next().await.unwrap();
    assert_eq!((c.args().unwrap().id, c.args().unwrap().reason), (id, Reason::ClosedByApp as u32));

    // A transient one runs out.
    let yes = Value::Bool(true);
    let id = proxy.notify("Volume", 0, "", "50%", "", &[], HashMap::from([("transient", &yes)]), -1).await.unwrap();
    assert_eq!(next(&mut said).await, Said::Raised { id, replaced: false });
    assert_eq!(next(&mut said).await, Said::Changed);
    let c = closed.next().await.unwrap();
    assert_eq!((c.args().unwrap().id, c.args().unwrap().reason), (id, Reason::Expired as u32));

    // What the Plugin tells, the application hears.
    server.invoked(7, "default").await;
    let i = invoked.next().await.unwrap();
    assert_eq!((i.args().unwrap().id, i.args().unwrap().action_key.as_str()), (7, "default"));

    // A second server waits for the name, and says who holds it.
    let (tx2, mut said2) = mpsc::unbounded_channel();
    let _second = Server::start(Arc::new(Mutex::new(Store::new(10))), tx2, Duration::from_millis(200)).await.unwrap();
    match next(&mut said2).await {
        Said::Held { held: false, line } => assert!(line.contains("is held by"), "{line}"),
        other => panic!("the second server holds nothing: {other:?}"),
    }
}
