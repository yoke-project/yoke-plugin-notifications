//! The notifications held: what applications raised and nobody has closed yet, newest first.
//!
//! A notification is kept until a person dismisses it, the application closes it, or it is pushed out by
//! newer ones past `keep`; one the application marks `transient` is also closed when its time runs out.
//! Every change counts the store's `sequence` once.

use serde_json::{json, Value};

/// The longest summary and body kept, in characters; the rest is cut.
pub const SUMMARY_MAX: usize = 512;
pub const BODY_MAX: usize = 4096;

/// Why a notification was closed, as the freedesktop protocol numbers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Expired = 1,
    Dismissed = 2,
    ClosedByApp = 3,
    Undefined = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

impl Urgency {
    pub fn of(byte: u8) -> Urgency {
        match byte {
            0 => Urgency::Low,
            2 => Urgency::Critical,
            _ => Urgency::Normal,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Urgency::Low => "low",
            Urgency::Normal => "normal",
            Urgency::Critical => "critical",
        }
    }
}

/// A notification as an application raises it.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub app: String,
    pub replaces: u32,
    pub icon: String,
    pub summary: String,
    pub body: String,
    /// Key and label, in order; the key `default` is the notification's own click.
    pub actions: Vec<(String, String)>,
    pub urgency: Urgency,
    pub desktop_entry: Option<String>,
    pub category: Option<String>,
    pub image: Option<String>,
    pub transient: bool,
    pub resident: bool,
    /// As the application asks: -1 the server's choice, 0 never, otherwise milliseconds.
    pub expire_ms: i32,
}

impl Default for Incoming {
    fn default() -> Self {
        Incoming {
            app: String::new(),
            replaces: 0,
            icon: String::new(),
            summary: String::new(),
            body: String::new(),
            actions: vec![],
            urgency: Urgency::Normal,
            desktop_entry: None,
            category: None,
            image: None,
            transient: false,
            resident: false,
            expire_ms: -1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub id: u32,
    pub incoming: Incoming,
    pub at: u64,
    pub seen: bool,
    /// Raised while do-not-disturb held, and not critical: kept, but not to be shown over the screen.
    pub quiet: bool,
    /// Counted at every replacement, so that a timer set for an older one closes nothing.
    pub generation: u64,
}

fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

impl Notification {
    pub fn json(&self) -> Value {
        let i = &self.incoming;
        let text = |s: &Option<String>| s.as_ref().filter(|s| !s.is_empty()).cloned();
        json!({
            "id": self.id,
            "app": i.app,
            "desktop_entry": text(&i.desktop_entry),
            "icon": if i.icon.is_empty() { None } else { Some(&i.icon) },
            "image": text(&i.image),
            "summary": i.summary,
            "body": i.body,
            "default_action": i.actions.iter().any(|(k, _)| k == "default"),
            "actions": i.actions.iter().filter(|(k, _)| k != "default")
                .map(|(k, l)| json!({ "key": k, "label": l })).collect::<Vec<_>>(),
            "urgency": i.urgency.as_str(),
            "category": text(&i.category),
            "at": self.at,
            "expire_ms": if i.expire_ms < 0 { None } else { Some(i.expire_ms) },
            "transient": i.transient,
            "resident": i.resident,
            "seen": self.seen,
            "quiet": self.quiet,
        })
    }
}

/// What a notification raised did to the store.
#[derive(Debug, PartialEq, Eq)]
pub struct Raised {
    pub id: u32,
    pub replaced: bool,
    pub generation: u64,
    /// Older notifications pushed out past `keep`.
    pub dropped: Vec<u32>,
}

#[derive(Debug)]
pub struct Store {
    pub keep: usize,
    pub dnd: bool,
    pub sequence: u64,
    next_id: u32,
    next_generation: u64,
    /// Newest first.
    list: Vec<Notification>,
}

impl Store {
    pub fn new(keep: usize) -> Store {
        Store { keep: keep.max(1), dnd: false, sequence: 0, next_id: 1, next_generation: 1, list: vec![] }
    }

    pub fn list(&self) -> &[Notification] {
        &self.list
    }

    pub fn get(&self, id: u32) -> Option<&Notification> {
        self.list.iter().find(|n| n.id == id)
    }

    pub fn unseen(&self) -> usize {
        self.list.iter().filter(|n| !n.seen).count()
    }

    fn changed(&mut self) {
        self.sequence += 1;
    }

    /// Takes a notification: a new one on top, or the one it replaces updated and brought to the top.
    pub fn raise(&mut self, mut incoming: Incoming, at: u64) -> Raised {
        incoming.summary = cut(&incoming.summary, SUMMARY_MAX);
        incoming.body = cut(&incoming.body, BODY_MAX);
        let quiet = self.dnd && incoming.urgency != Urgency::Critical;
        let generation = self.next_generation;
        self.next_generation += 1;
        let replaced = incoming.replaces != 0 && self.get(incoming.replaces).is_some();
        let id = if replaced {
            let id = incoming.replaces;
            self.list.retain(|n| n.id != id);
            id
        } else {
            let id = self.next_id;
            // 0 is no notification to the protocol: the count skips it as it wraps.
            self.next_id = self.next_id.checked_add(1).unwrap_or(1);
            id
        };
        self.list.insert(0, Notification { id, incoming, at, seen: false, quiet, generation });
        let dropped = if self.list.len() > self.keep {
            self.list.split_off(self.keep).into_iter().map(|n| n.id).collect()
        } else {
            vec![]
        };
        self.changed();
        Raised { id, replaced, generation, dropped }
    }

    /// Closes one; `None` where there is none of that id.
    pub fn close(&mut self, id: u32) -> Option<Notification> {
        let at = self.list.iter().position(|n| n.id == id)?;
        let n = self.list.remove(at);
        self.changed();
        Some(n)
    }

    /// Closes one whose time ran out, if it is still the one the timer was set for.
    pub fn expire(&mut self, id: u32, generation: u64) -> bool {
        if self.get(id).is_some_and(|n| n.generation == generation) {
            self.close(id);
            true
        } else {
            false
        }
    }

    /// Closes every one, or every one of an application; returns the ids closed.
    pub fn close_all(&mut self, app: Option<&str>) -> Vec<u32> {
        let (gone, kept): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.list).into_iter().partition(|n| app.is_none_or(|a| n.incoming.app == a));
        self.list = kept;
        if !gone.is_empty() {
            self.changed();
        }
        gone.into_iter().map(|n| n.id).collect()
    }

    /// Marks every one seen; says whether anything changed.
    pub fn see(&mut self) -> bool {
        let mut any = false;
        for n in self.list.iter_mut().filter(|n| !n.seen) {
            n.seen = true;
            any = true;
        }
        if any {
            self.changed();
        }
        any
    }

    pub fn set_dnd(&mut self, on: bool) -> bool {
        if self.dnd == on {
            return false;
        }
        self.dnd = on;
        self.changed();
        true
    }
}
