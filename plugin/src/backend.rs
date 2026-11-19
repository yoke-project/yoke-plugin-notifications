//! The notification server on the session's bus: `org.freedesktop.Notifications`, as the freedesktop
//! protocol (1.2) has it. Applications call it to raise and close their notifications; it tells them when one
//! is closed and when a person picked one of its actions. Behind a trait, so that a test stands in for it.
//!
//! **One server holds the name.** Where another does — mako, dunst, swaync — the Plugin waits in the bus's
//! queue for it, says who holds it, and takes it the moment that one lets go.
//!
//! A Yoke unit is launched with no environment beyond its own, so without `DBUS_SESSION_BUS_ADDRESS` the
//! account's bus is looked for where systemd puts it, `/run/user/<uid>/bus`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::names::BusName;
use zbus::Connection;
use zvariant::OwnedValue;

use crate::store::{Incoming, Reason, Store, Urgency};

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";
pub const SPEC: &str = "1.2";

/// What the server says to the Plugin.
#[derive(Debug, PartialEq, Eq)]
pub enum Said {
    /// A notification was raised, or one replaced.
    Raised { id: u32, replaced: bool },
    /// The store changed some other way: an application closed one, or a transient one ran out.
    Changed,
    /// The name was taken or lost; the line says which, and who holds it where the Plugin does not.
    Held { held: bool, line: String },
}

/// What the Plugin tells applications, through the server.
#[async_trait]
pub trait Bus: Send + Sync + 'static {
    /// A notification was closed, and why.
    async fn closed(&self, id: u32, reason: Reason);
    /// A person picked one of a notification's actions.
    async fn invoked(&self, id: u32, action: &str);
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

async fn session_bus() -> zbus::Result<zbus::connection::Builder<'static>> {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
        return zbus::connection::Builder::session();
    }
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
    zbus::connection::Builder::address(format!("unix:path=/run/user/{uid}/bus").as_str())
}

/// The server's object: what applications call.
struct Daemon {
    store: Arc<Mutex<Store>>,
    said: UnboundedSender<Said>,
    /// How long a transient notification that names no time of its own is kept.
    transient: Duration,
}

fn text(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    hints.get(key).and_then(|v| <&str>::try_from(&**v).ok()).map(str::to_owned)
}

fn flag(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    hints.get(key).and_then(|v| bool::try_from(&**v).ok()).unwrap_or(false)
}

async fn emit_closed(conn: &Connection, id: u32, reason: Reason) {
    let _ = conn.emit_signal(None::<BusName>, PATH, NAME, "NotificationClosed", &(id, reason as u32)).await;
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Daemon {
    async fn get_capabilities(&self) -> Vec<String> {
        ["actions", "body", "persistence"].iter().map(|s| s.to_string()).collect()
    }

    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        #[zbus(connection)] conn: &Connection,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let incoming = Incoming {
            app: app_name,
            replaces: replaces_id,
            icon: app_icon,
            summary,
            body,
            actions: actions.chunks(2).filter(|p| p.len() == 2).map(|p| (p[0].clone(), p[1].clone())).collect(),
            urgency: Urgency::of(hints.get("urgency").and_then(|v| u8::try_from(&**v).ok()).unwrap_or(1)),
            desktop_entry: text(&hints, "desktop-entry"),
            category: text(&hints, "category"),
            image: text(&hints, "image-path").or_else(|| text(&hints, "image_path")),
            transient: flag(&hints, "transient"),
            resident: flag(&hints, "resident"),
            expire_ms: expire_timeout.max(-1),
        };
        let (transient, expire_ms) = (incoming.transient, incoming.expire_ms);
        let raised = self.store.lock().unwrap().raise(incoming, now_ms());
        for id in &raised.dropped {
            emit_closed(conn, *id, Reason::Undefined).await;
        }
        if transient && expire_ms != 0 {
            let wait = if expire_ms > 0 { Duration::from_millis(expire_ms as u64) } else { self.transient };
            let (store, said, conn, id, generation) =
                (self.store.clone(), self.said.clone(), conn.clone(), raised.id, raised.generation);
            tokio::spawn(async move {
                tokio::time::sleep(wait).await;
                let expired = store.lock().unwrap().expire(id, generation);
                if expired {
                    emit_closed(&conn, id, Reason::Expired).await;
                    let _ = said.send(Said::Changed);
                }
            });
        }
        let _ = self.said.send(Said::Raised { id: raised.id, replaced: raised.replaced });
        raised.id
    }

    async fn close_notification(&self, #[zbus(connection)] conn: &Connection, id: u32) {
        let closed = self.store.lock().unwrap().close(id).is_some();
        if closed {
            emit_closed(conn, id, Reason::ClosedByApp).await;
            let _ = self.said.send(Said::Changed);
        }
    }

    async fn get_server_information(&self) -> (String, String, String, String) {
        ("yoke-notifications".into(), "Yoke".into(), env!("CARGO_PKG_VERSION").into(), SPEC.into())
    }
}

pub struct Server {
    conn: Connection,
}

#[async_trait]
impl Bus for Server {
    async fn closed(&self, id: u32, reason: Reason) {
        emit_closed(&self.conn, id, reason).await;
    }
    async fn invoked(&self, id: u32, action: &str) {
        let _ = self.conn.emit_signal(None::<BusName>, PATH, NAME, "ActionInvoked", &(id, action)).await;
    }
}

/// Who holds the name, as a person would name it: the program, and its process.
async fn holder(dbus: &DBusProxy<'_>) -> String {
    let Ok(name) = BusName::try_from(NAME) else { return "another server".into() };
    let Ok(owner) = dbus.get_name_owner(name).await else { return "another server".into() };
    match dbus.get_connection_unix_process_id(owner.clone().into()).await {
        Ok(pid) => {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            let comm = comm.trim();
            if comm.is_empty() { format!("process {pid}") } else { format!("{comm} (process {pid})") }
        }
        Err(_) => owner.to_string(),
    }
}

impl Server {
    /// Serves the object on the session's bus and asks for the name, queueing behind a server that holds it.
    /// What follows — the name taken or lost — is said on `said`.
    pub async fn start(store: Arc<Mutex<Store>>, said: UnboundedSender<Said>, transient: Duration) -> Result<Arc<Server>, String> {
        let daemon = Daemon { store, said: said.clone(), transient };
        let conn = session_bus()
            .await
            .and_then(|b| b.serve_at(PATH, daemon))
            .map_err(|e| format!("the session's bus cannot be reached: {e}"))?
            .build()
            .await
            .map_err(|e| format!("the session's bus cannot be reached: {e}"))?;
        let dbus = DBusProxy::new(&conn).await.map_err(|e| e.to_string())?;
        let mut acquired = dbus.receive_name_acquired().await.map_err(|e| e.to_string())?;
        let mut lost = dbus.receive_name_lost().await.map_err(|e| e.to_string())?;
        let reply = dbus
            .request_name(NAME.try_into().map_err(|e: zbus::names::Error| e.to_string())?, RequestNameFlags::ReplaceExisting.into())
            .await
            .map_err(|e| format!("{NAME} cannot be asked for: {e}"))?;
        let mut held = matches!(reply, RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner);
        let first = if held {
            Said::Held { held, line: format!("{NAME} held") }
        } else {
            Said::Held { held, line: format!("{NAME} is held by {}; waiting for it", holder(&dbus).await) }
        };
        let _ = said.send(first);
        // The bus also says what the request already answered: only a change is passed on.
        let watching = dbus.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(s) = acquired.next() => {
                        if !held && s.args().is_ok_and(|a| a.name() == NAME) {
                            held = true;
                            let _ = said.send(Said::Held { held, line: format!("{NAME} held") });
                        }
                    }
                    Some(s) = lost.next() => {
                        if held && s.args().is_ok_and(|a| a.name() == NAME) {
                            held = false;
                            let line = format!("{NAME} was taken by {}", holder(&watching).await);
                            let _ = said.send(Said::Held { held, line });
                        }
                    }
                    else => return,
                }
            }
        });
        Ok(Arc::new(Server { conn }))
    }
}
