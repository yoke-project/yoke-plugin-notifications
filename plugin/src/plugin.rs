//! The notifications Plugin: what applications raise, as a question, a stream, commands and an occurrence.
//!
//! - **`notifications.list` is a question and a stream of the same record**, with a sequence: the server's
//!   state, do-not-disturb, how many are unseen, and every notification held, newest first. A client that
//!   subscribes and then asks holds the present, and drops what the stream brings at or below the sequence it
//!   was answered with.
//! - **A command is acknowledged once the store has taken it**, and the application told where the
//!   protocol says so: a dismissal as closed by the person, an action as invoked.
//! - **`notifications.received` says one came, never what it says**: the application and the urgency. An
//!   occurrence is kept in the log store, and a notification's text is a person's correspondence, so the
//!   text travels on the stream and in the answer, which are not kept.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use yoke_sdk::plugin::{Capability, Command, Declaration, Event, Object, Outcome, Question, Severity, Stream, Unit};

use crate::backend::{Bus, Said};
use crate::store::{Reason, Store, Urgency};

pub const PLUGIN: &str = "com.yoke.notifications";
/// The format every answer, frame and detail states as `schema`.
pub const SCHEMA: u32 = 1;

pub const LIST: &str = "notifications.list";
pub const QUERIES: [&str; 1] = [LIST];
pub const STREAMS: [&str; 1] = [LIST];

pub const DISMISS: &str = "notifications.dismiss";
pub const DISMISS_ALL: &str = "notifications.dismiss.all";
pub const INVOKE: &str = "notifications.invoke";
pub const SEEN: &str = "notifications.seen";
pub const DND_SET: &str = "notifications.dnd.set";
pub const COMMANDS: [&str; 5] = [DISMISS, DISMISS_ALL, INVOKE, SEEN, DND_SET];

pub const RECEIVED: &str = "notifications.received";
pub const OCCURRENCES: [&str; 1] = [RECEIVED];

/// How many notifications are kept, and how long a transient one that names no time is shown, unless the
/// unit's arguments say otherwise.
pub const KEEP: usize = 200;
pub const TRANSIENT_MS: u64 = 5000;

pub fn declaration() -> Declaration {
    let answer = |q: &&str| Capability { name: format!("query.{q}.answer"), governs: Object::Query(q.to_string()) };
    let accept = |c: &&str| Capability { name: format!("command.{c}.accept"), governs: Object::Command(c.to_string()) };
    let publish = |s: &&str| Capability { name: format!("stream.{s}.publish"), governs: Object::Stream(s.to_string()) };
    let report = |o: &&str| Capability { name: format!("event.{o}.report"), governs: Object::Occurrence(o.to_string()) };
    Declaration {
        id: PLUGIN.into(),
        queries: QUERIES.iter().map(|q| q.to_string()).collect(),
        commands: COMMANDS.iter().map(|c| c.to_string()).collect(),
        // The whole list again at every change, so a lost frame is made good by the next.
        streams: vec![Stream { id: LIST.into(), tolerates_loss: true, tolerates_reorder: false, max_frame: 0 }],
        occurrences: OCCURRENCES.iter().map(|o| o.to_string()).collect(),
        capabilities: QUERIES
            .iter()
            .map(answer)
            .chain(COMMANDS.iter().map(accept))
            .chain(STREAMS.iter().map(publish))
            .chain(OCCURRENCES.iter().map(report))
            .collect(),
        ..Default::default()
    }
}

/// What the unit's arguments choose: `--keep N`, `--transient-ms N`.
#[derive(Debug, PartialEq, Eq)]
pub struct Settings {
    pub keep: usize,
    pub transient_ms: u64,
}

impl Settings {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Settings, String> {
        let mut s = Settings { keep: KEEP, transient_ms: TRANSIENT_MS };
        let mut it = args.into_iter();
        while let Some(flag) = it.next() {
            let mut number = || -> Result<u64, String> {
                it.next().and_then(|v| v.parse().ok()).ok_or(format!("{flag} takes a number"))
            };
            match flag.as_str() {
                "--keep" => s.keep = (number()? as usize).max(1),
                "--transient-ms" => s.transient_ms = number()?,
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(s)
    }
}

#[derive(Debug)]
pub enum Error {
    Invalid(String),
    NotFound(String),
    Unavailable(String),
}

pub fn code(e: &Error) -> &'static str {
    match e {
        Error::Invalid(_) => "invalid",
        Error::NotFound(_) => "not_found",
        Error::Unavailable(_) => "unavailable",
    }
}

pub fn message(e: &Error) -> String {
    match e {
        Error::Invalid(m) | Error::NotFound(m) | Error::Unavailable(m) => m.clone(),
    }
}

fn input<T: DeserializeOwned>(what: &str, payload: &[u8]) -> Result<T, Error> {
    let payload = if payload.iter().all(u8::is_ascii_whitespace) { b"{}".as_slice() } else { payload };
    serde_json::from_slice(payload).map_err(|e| {
        let kind = match e.classify() {
            serde_json::error::Category::Data => "a field is missing, unknown or of the wrong type",
            _ => "it is not JSON",
        };
        Error::Invalid(format!("the payload is not what {what} takes: {kind}, at line {} column {}", e.line(), e.column()))
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoInput {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdInput {
    pub id: u32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AllInput {
    /// Only this application's, where given.
    pub app: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvokeInput {
    pub id: u32,
    /// The action's key; `default` is the notification's own click.
    #[serde(default = "default_action")]
    pub action: String,
}

fn default_action() -> String {
    "default".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DndInput {
    pub on: bool,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// What the Plugin does on the Session, behind a trait so that a test can stand in for the Core.
pub trait Acts {
    fn answer(&self, q: &Question, payload: Vec<u8>) -> impl std::future::Future<Output = Result<(), yoke_sdk::base::Error>>;
    fn fail(&self, q: &Question, code: &str, message: &str) -> impl std::future::Future<Output = Result<(), yoke_sdk::base::Error>>;
    fn ack(&self, c: &Command, outcome: Outcome, line: &str) -> impl std::future::Future<Output = Result<(), yoke_sdk::base::Error>>;
    /// An occurrence, its facts as JSON in its detail.
    fn report(&self, occurrence: &str, severity: u8, line: &str, detail: Value) -> impl std::future::Future<Output = Result<(), yoke_sdk::base::Error>>;
    fn emit(&self, stream: &str, payload: Vec<u8>) -> impl std::future::Future<Output = Result<(), yoke_sdk::base::Error>>;
}

impl Acts for Unit {
    async fn answer(&self, q: &Question, payload: Vec<u8>) -> Result<(), yoke_sdk::base::Error> {
        Unit::answer(self, q, payload).await
    }
    async fn fail(&self, q: &Question, code: &str, message: &str) -> Result<(), yoke_sdk::base::Error> {
        Unit::fail(self, &q.id, code, message).await
    }
    async fn ack(&self, c: &Command, outcome: Outcome, line: &str) -> Result<(), yoke_sdk::base::Error> {
        Unit::ack(self, c, outcome, line).await
    }
    async fn report(&self, occurrence: &str, severity: u8, line: &str, detail: Value) -> Result<(), yoke_sdk::base::Error> {
        Unit::report(self, occurrence, Some(Severity::of(severity)), line, detail.to_string().into_bytes()).await
    }
    async fn emit(&self, stream: &str, payload: Vec<u8>) -> Result<(), yoke_sdk::base::Error> {
        Unit::emit(self, stream, payload).await
    }
}

/// The server's state as a person reads it.
struct Server {
    held: bool,
    line: String,
}

pub struct Notifications {
    pub store: Arc<Mutex<Store>>,
    bus: Mutex<Option<Arc<dyn Bus>>>,
    server: Mutex<Server>,
    /// The sequence last emitted, so that a record is emitted once.
    emitted: AtomicU64,
    flows: AtomicBool,
    /// One record emitted at a time, so that frames leave in the order of their sequence.
    publishing: tokio::sync::Mutex<()>,
}

impl Notifications {
    pub fn new(keep: usize) -> Arc<Notifications> {
        Arc::new(Notifications {
            store: Arc::new(Mutex::new(Store::new(keep))),
            bus: Mutex::new(None),
            server: Mutex::new(Server { held: false, line: "the session's bus is not reached yet".into() }),
            emitted: AtomicU64::new(0),
            flows: AtomicBool::new(false),
            publishing: tokio::sync::Mutex::new(()),
        })
    }

    /// The server, once it serves: what tells applications.
    pub fn serve(&self, bus: Arc<dyn Bus>) {
        *self.bus.lock().unwrap() = Some(bus);
    }

    fn bus(&self) -> Option<Arc<dyn Bus>> {
        self.bus.lock().unwrap().clone()
    }

    /// Where the server stands, as its health says it. Counted as a change of the record.
    pub fn held(&self, held: bool, line: &str) {
        *self.server.lock().unwrap() = Server { held, line: line.to_string() };
        self.store.lock().unwrap().sequence += 1;
    }

    pub fn record(&self) -> Value {
        let store = self.store.lock().unwrap();
        let server = self.server.lock().unwrap();
        json!({
            "schema": SCHEMA,
            "sequence": store.sequence,
            "at": now_ms(),
            "server": { "held": server.held, "line": server.line },
            "dnd": store.dnd,
            "unseen": store.unseen(),
            "notifications": store.list().iter().map(|n| n.json()).collect::<Vec<_>>(),
        })
    }

    /// Emits the record where the stream flows and it changed since it was last emitted.
    pub async fn publish(&self, u: &impl Acts) -> Result<(), yoke_sdk::base::Error> {
        if !self.flows.load(Ordering::Relaxed) {
            return Ok(());
        }
        let _one = self.publishing.lock().await;
        let record = self.record();
        let sequence = record["sequence"].as_u64().unwrap_or(0);
        if self.emitted.fetch_max(sequence, Ordering::Relaxed) >= sequence {
            return Ok(());
        }
        u.emit(LIST, record.to_string().into_bytes()).await
    }

    /// What the server said: the record published, and a notification come reported.
    pub async fn said(&self, u: &impl Acts, said: &Said) -> Result<(), yoke_sdk::base::Error> {
        let published = self.publish(u).await;
        let Said::Raised { id, replaced: false } = said else { return published };
        let Some((app, urgency)) = self.store.lock().unwrap().get(*id).map(|n| (n.incoming.app.clone(), n.incoming.urgency))
        else {
            return published;
        };
        let who = if app.is_empty() { "an application".to_string() } else { app.clone() };
        let line = format!("a notification from {who}");
        let detail = json!({ "schema": SCHEMA, "line": line, "id": id, "app": app, "urgency": urgency.as_str() });
        let severity = if urgency == Urgency::Critical { 30 } else { 10 };
        published.and(u.report(RECEIVED, severity, &line, detail).await)
    }

    /// The stream was activated: it is given the present record at once.
    pub async fn activated(&self, u: &impl Acts, stream: &str) -> Result<(), yoke_sdk::base::Error> {
        if stream != LIST {
            return Ok(());
        }
        self.flows.store(true, Ordering::Relaxed);
        let _one = self.publishing.lock().await;
        let record = self.record();
        self.emitted.fetch_max(record["sequence"].as_u64().unwrap_or(0), Ordering::Relaxed);
        u.emit(LIST, record.to_string().into_bytes()).await
    }

    pub fn stopped(&self, stream: &str) {
        if stream == LIST {
            self.flows.store(false, Ordering::Relaxed);
        }
    }

    pub fn answer(&self, query: &str, payload: &[u8]) -> Result<Vec<u8>, Error> {
        let _: NoInput = input(query, payload)?;
        match query {
            LIST => Ok(self.record().to_string().into_bytes()),
            other => Err(Error::Invalid(format!("{other} is not a question of {PLUGIN}"))),
        }
    }

    /// Carries a command out, and returns the line it is acknowledged with.
    pub async fn act(&self, command: &str, payload: &[u8]) -> Result<String, Error> {
        let missing = |id: u32| Error::NotFound(format!("there is no notification of id {id}"));
        match command {
            DISMISS => {
                let i: IdInput = input(command, payload)?;
                self.store.lock().unwrap().close(i.id).ok_or_else(|| missing(i.id))?;
                if let Some(bus) = self.bus() {
                    bus.closed(i.id, Reason::Dismissed).await;
                }
                Ok(format!("notification {} dismissed", i.id))
            }
            DISMISS_ALL => {
                let i: AllInput = input(command, payload)?;
                let gone = self.store.lock().unwrap().close_all(i.app.as_deref());
                if let Some(bus) = self.bus() {
                    for id in &gone {
                        bus.closed(*id, Reason::Dismissed).await;
                    }
                }
                Ok(format!("{} notification{} dismissed", gone.len(), if gone.len() == 1 { "" } else { "s" }))
            }
            INVOKE => {
                let i: InvokeInput = input(command, payload)?;
                let bus = self.bus().ok_or_else(|| Error::Unavailable("the server does not serve: no application can be told".into()))?;
                let resident = {
                    let store = self.store.lock().unwrap();
                    let n = store.get(i.id).ok_or_else(|| missing(i.id))?;
                    let offered = n.incoming.actions.iter().any(|(k, _)| *k == i.action);
                    if !offered {
                        return Err(Error::NotFound(format!("notification {} offers no action {}", i.id, i.action)));
                    }
                    n.incoming.resident
                };
                bus.invoked(i.id, &i.action).await;
                // An action taken is the notification answered, unless the application keeps it.
                if !resident && self.store.lock().unwrap().close(i.id).is_some() {
                    bus.closed(i.id, Reason::Dismissed).await;
                }
                Ok(format!("action {} of notification {} invoked", i.action, i.id))
            }
            SEEN => {
                let _: NoInput = input(command, payload)?;
                self.store.lock().unwrap().see();
                Ok("every notification seen".into())
            }
            DND_SET => {
                let i: DndInput = input(command, payload)?;
                self.store.lock().unwrap().set_dnd(i.on);
                Ok(format!("do-not-disturb {}", if i.on { "on" } else { "off" }))
            }
            other => Err(Error::Invalid(format!("{other} is not a command of {PLUGIN}"))),
        }
    }
}

pub async fn handle(u: &impl Acts, event: &Event, n: &Notifications) -> Result<(), yoke_sdk::base::Error> {
    match event {
        Event::Question(q) if QUERIES.contains(&q.r#type.as_str()) => match n.answer(&q.r#type, &q.payload) {
            Ok(answer) => u.answer(q, answer).await,
            Err(e) => u.fail(q, code(&e), &message(&e)).await,
        },
        Event::Command(c) if COMMANDS.contains(&c.r#type.as_str()) => match n.act(&c.r#type, &c.payload).await {
            Ok(line) => u.ack(c, Outcome::Done, &line).await.and(n.publish(u).await),
            Err(e) => u.ack(c, Outcome::Failed, &format!("{}: {}", code(&e), message(&e))).await,
        },
        _ => Ok(()),
    }
}
