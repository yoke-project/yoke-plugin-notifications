//! The Plugin with a recorder for the Core and a recorder for the bus: notifications raised as the server
//! would raise them, and what the Plugin answers, emits, reports and tells applications.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use notifications::backend::{Bus, Said};
use notifications::plugin::*;
use notifications::store::{Incoming, Reason, Store, Urgency, BODY_MAX};
use serde_json::{json, Value};
use yoke_sdk::plugin::{Command, Event, Outcome, Question};

#[derive(Default)]
struct Core {
    said: Mutex<Vec<String>>,
}

impl Core {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.said.lock().unwrap())
    }
    fn push(&self, line: String) -> Result<(), yoke_sdk::base::Error> {
        self.said.lock().unwrap().push(line);
        Ok(())
    }
}

impl Acts for Core {
    async fn answer(&self, _q: &Question, payload: Vec<u8>) -> Result<(), yoke_sdk::base::Error> {
        self.push(format!("answer {}", String::from_utf8_lossy(&payload)))
    }
    async fn fail(&self, _q: &Question, code: &str, message: &str) -> Result<(), yoke_sdk::base::Error> {
        self.push(format!("fail {code}: {message}"))
    }
    async fn ack(&self, _c: &Command, outcome: Outcome, line: &str) -> Result<(), yoke_sdk::base::Error> {
        self.push(format!("ack {outcome:?}: {line}"))
    }
    async fn report(&self, occurrence: &str, severity: u8, line: &str, detail: Value) -> Result<(), yoke_sdk::base::Error> {
        assert_eq!(detail["line"], line, "the detail carries the line a client reads");
        self.push(format!("report {occurrence} {severity}: {detail}"))
    }
    async fn emit(&self, stream: &str, payload: Vec<u8>) -> Result<(), yoke_sdk::base::Error> {
        self.push(format!("emit {stream} {}", String::from_utf8_lossy(&payload)))
    }
}

/// The applications, as the server would tell them.
#[derive(Default)]
struct Apps {
    told: Mutex<Vec<String>>,
}

impl Apps {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.told.lock().unwrap())
    }
}

#[async_trait]
impl Bus for Apps {
    async fn closed(&self, id: u32, reason: Reason) {
        self.told.lock().unwrap().push(format!("closed {id} {reason:?}"));
    }
    async fn invoked(&self, id: u32, action: &str) {
        self.told.lock().unwrap().push(format!("invoked {id} {action}"));
    }
}

fn question(r#type: &str) -> Event {
    Event::Question(Question { id: "q-1".into(), r#type: r#type.into(), payload: b"{}".to_vec() })
}

fn command(r#type: &str, payload: Value) -> Event {
    Event::Command(Command { id: "c-1".into(), r#type: r#type.into(), payload: payload.to_string().into_bytes() })
}

fn json_of(line: &str, skip: usize) -> Value {
    serde_json::from_str(line.splitn(skip + 1, ' ').nth(skip).unwrap()).unwrap()
}

fn mail(summary: &str) -> Incoming {
    Incoming {
        app: "Thunderbird".into(),
        summary: summary.into(),
        body: "Lunch tomorrow?".into(),
        actions: vec![("default".into(), "Open".into()), ("archive".into(), "Archive".into())],
        desktop_entry: Some("org.mozilla.Thunderbird".into()),
        ..Default::default()
    }
}

/// Raises as the server does: into the store, then said.
async fn raise(core: &Core, n: &Notifications, incoming: Incoming) -> u32 {
    let raised = n.store.lock().unwrap().raise(incoming, 1000);
    n.said(core, &Said::Raised { id: raised.id, replaced: raised.replaced }).await.unwrap();
    raised.id
}

fn plugin() -> (Core, Arc<Apps>, Arc<Notifications>) {
    let n = Notifications::new(KEEP);
    let apps = Arc::new(Apps::default());
    n.serve(apps.clone());
    n.held(true, "org.freedesktop.Notifications held");
    (Core::default(), apps, n)
}

#[test]
fn the_manifest_kept_is_the_manifest_declared() {
    assert_eq!(declaration().manifest(), include_str!("../com.yoke.notifications.yaml"));
}

#[test]
fn every_object_declared_has_one_capability_and_nothing_is_needed() {
    let d = declaration();
    assert_eq!(d.capabilities.len(), d.queries.len() + d.commands.len() + d.streams.len() + d.occurrences.len());
    assert!(d.needs.is_empty());
}

#[test]
fn the_arguments_choose_what_is_kept_and_for_how_long() {
    let args = |a: &[&str]| Settings::parse(a.iter().map(|s| s.to_string()));
    assert_eq!(args(&[]).unwrap(), Settings { keep: KEEP, transient_ms: TRANSIENT_MS });
    assert_eq!(args(&["--keep", "20", "--transient-ms", "3000"]).unwrap(), Settings { keep: 20, transient_ms: 3000 });
    assert!(args(&["--keep"]).is_err());
    assert!(args(&["--loud"]).is_err());
}

#[test]
fn a_replacement_keeps_its_id_and_comes_to_the_top() {
    let mut s = Store::new(10);
    let a = s.raise(mail("a"), 1).id;
    let b = s.raise(mail("b"), 2).id;
    assert_eq!((a, b), (1, 2));
    let again = s.raise(Incoming { replaces: a, ..mail("a, again") }, 3);
    assert_eq!((again.id, again.replaced), (a, true));
    assert_eq!(s.list().iter().map(|n| n.id).collect::<Vec<_>>(), vec![a, b]);
    // One that replaces what is gone is new.
    let gone = s.raise(Incoming { replaces: 99, ..mail("c") }, 4);
    assert_eq!((gone.id, gone.replaced), (3, false));
    assert_eq!(s.sequence, 4, "every change counted once");
}

#[test]
fn past_keep_the_oldest_are_pushed_out() {
    let mut s = Store::new(2);
    s.raise(mail("a"), 1);
    s.raise(mail("b"), 2);
    let r = s.raise(mail("c"), 3);
    assert_eq!(r.dropped, vec![1]);
    assert_eq!(s.list().len(), 2);
}

#[test]
fn a_timer_for_a_replaced_notification_closes_nothing() {
    let mut s = Store::new(10);
    let first = s.raise(mail("a"), 1);
    let second = s.raise(Incoming { replaces: first.id, ..mail("b") }, 2);
    assert!(!s.expire(first.id, first.generation));
    assert!(s.expire(second.id, second.generation));
    assert!(s.list().is_empty());
}

#[test]
fn a_long_body_is_cut() {
    let mut s = Store::new(10);
    let id = s.raise(Incoming { body: "x".repeat(BODY_MAX + 50), ..mail("a") }, 1).id;
    assert_eq!(s.get(id).unwrap().incoming.body.chars().count(), BODY_MAX + 1);
}

#[test]
fn do_not_disturb_quiets_all_but_the_critical() {
    let mut s = Store::new(10);
    s.set_dnd(true);
    let calm = s.raise(mail("a"), 1).id;
    let loud = s.raise(Incoming { urgency: Urgency::Critical, ..mail("b") }, 2).id;
    assert!(s.get(calm).unwrap().quiet);
    assert!(!s.get(loud).unwrap().quiet);
}

#[tokio::test]
async fn the_record_holds_the_notification_and_the_occurrence_never_its_text() {
    let (core, _apps, n) = plugin();
    n.activated(&core, LIST).await.unwrap();
    core.take();
    let id = raise(&core, &n, mail("Dinner")).await;
    let said = core.take();
    assert_eq!(said.len(), 2, "{said:?}");
    let record = json_of(&said[0], 2);
    assert_eq!(record["schema"], SCHEMA);
    assert_eq!(record["unseen"], 1);
    assert_eq!(record["server"]["held"], true);
    let one = &record["notifications"][0];
    assert_eq!(one["id"], id);
    assert_eq!(one["summary"], "Dinner");
    assert_eq!(one["default_action"], true);
    assert_eq!(one["actions"], json!([{ "key": "archive", "label": "Archive" }]));
    assert_eq!(one["urgency"], "normal");
    assert_eq!(one["expire_ms"], Value::Null);
    assert!(said[1].starts_with("report notifications.received 10: "), "{}", said[1]);
    assert!(!said[1].contains("Dinner") && !said[1].contains("Lunch"), "no text in an occurrence: {}", said[1]);
    assert!(said[1].contains("Thunderbird"));
}

#[tokio::test]
async fn a_replacement_is_emitted_and_not_reported() {
    let (core, _apps, n) = plugin();
    n.activated(&core, LIST).await.unwrap();
    let id = raise(&core, &n, mail("a")).await;
    core.take();
    raise(&core, &n, Incoming { replaces: id, ..mail("b") }).await;
    let said = core.take();
    assert_eq!(said.len(), 1);
    assert!(said[0].starts_with("emit notifications.list "));
}

#[tokio::test]
async fn nothing_is_emitted_where_the_stream_does_not_flow_and_the_answer_holds_the_present() {
    let (core, _apps, n) = plugin();
    raise(&core, &n, mail("a")).await;
    let said = core.take();
    assert_eq!(said.len(), 1, "the occurrence alone: {said:?}");
    handle(&core, &question(LIST), &n).await.unwrap();
    let answer = json_of(&core.take()[0], 1);
    assert_eq!(answer["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(answer["sequence"], n.store.lock().unwrap().sequence);
}

#[tokio::test]
async fn a_dismissal_tells_the_application() {
    let (core, apps, n) = plugin();
    let id = raise(&core, &n, mail("a")).await;
    core.take();
    handle(&core, &command(DISMISS, json!({ "id": id })), &n).await.unwrap();
    assert_eq!(core.take(), vec![format!("ack Done: notification {id} dismissed")]);
    assert_eq!(apps.take(), vec![format!("closed {id} Dismissed")]);
    handle(&core, &command(DISMISS, json!({ "id": id })), &n).await.unwrap();
    assert_eq!(core.take(), vec![format!("ack Failed: not_found: there is no notification of id {id}")]);
}

#[tokio::test]
async fn an_action_is_invoked_and_closes_the_notification_unless_resident() {
    let (core, apps, n) = plugin();
    let id = raise(&core, &n, mail("a")).await;
    let kept = raise(&core, &n, Incoming { resident: true, ..mail("b") }).await;
    core.take();
    handle(&core, &command(INVOKE, json!({ "id": id })), &n).await.unwrap();
    assert_eq!(apps.take(), vec![format!("invoked {id} default"), format!("closed {id} Dismissed")]);
    handle(&core, &command(INVOKE, json!({ "id": kept, "action": "archive" })), &n).await.unwrap();
    assert_eq!(apps.take(), vec![format!("invoked {kept} archive")]);
    assert!(n.store.lock().unwrap().get(kept).is_some());
    core.take();
    handle(&core, &command(INVOKE, json!({ "id": kept, "action": "reply" })), &n).await.unwrap();
    assert_eq!(core.take(), vec![format!("ack Failed: not_found: notification {kept} offers no action reply")]);
}

#[tokio::test]
async fn dismissing_all_of_an_application_leaves_the_others() {
    let (core, apps, n) = plugin();
    raise(&core, &n, mail("a")).await;
    let other = raise(&core, &n, Incoming { app: "Teams".into(), ..mail("b") }).await;
    raise(&core, &n, mail("c")).await;
    core.take();
    handle(&core, &command(DISMISS_ALL, json!({ "app": "Thunderbird" })), &n).await.unwrap();
    assert_eq!(core.take(), vec!["ack Done: 2 notifications dismissed".to_string()]);
    assert_eq!(apps.take().len(), 2);
    assert_eq!(n.store.lock().unwrap().list().iter().map(|n| n.id).collect::<Vec<_>>(), vec![other]);
}

#[tokio::test]
async fn seen_and_do_not_disturb_change_the_record() {
    let (core, _apps, n) = plugin();
    raise(&core, &n, mail("a")).await;
    n.activated(&core, LIST).await.unwrap();
    core.take();
    handle(&core, &command(SEEN, json!({})), &n).await.unwrap();
    let said = core.take();
    assert_eq!(json_of(&said[1], 2)["unseen"], 0);
    handle(&core, &command(DND_SET, json!({ "on": true })), &n).await.unwrap();
    let said = core.take();
    assert_eq!(said[0], "ack Done: do-not-disturb on");
    assert_eq!(json_of(&said[1], 2)["dnd"], true);
    // Asked again, nothing changes and nothing is emitted.
    handle(&core, &command(DND_SET, json!({ "on": true })), &n).await.unwrap();
    assert_eq!(core.take().len(), 1);
}

#[tokio::test]
async fn a_payload_that_does_not_parse_is_described_by_where_it_fails() {
    let (core, _apps, n) = plugin();
    handle(&core, &command(DISMISS, json!({ "id": "one" })), &n).await.unwrap();
    let said = core.take();
    assert!(said[0].starts_with("ack Failed: invalid: the payload is not what notifications.dismiss takes"), "{}", said[0]);
}

#[tokio::test]
async fn without_a_server_an_action_cannot_be_invoked_and_the_record_says_why() {
    let n = Notifications::new(KEEP);
    let core = Core::default();
    n.held(false, "the session's bus cannot be reached: no socket");
    let id = raise(&core, &n, mail("a")).await;
    core.take();
    handle(&core, &command(INVOKE, json!({ "id": id })), &n).await.unwrap();
    assert!(core.take()[0].starts_with("ack Failed: unavailable:"));
    handle(&core, &question(LIST), &n).await.unwrap();
    let answer = json_of(&core.take()[0], 1);
    assert_eq!(answer["server"]["held"], false);
    assert_eq!(answer["server"]["line"], "the session's bus cannot be reached: no socket");
}
