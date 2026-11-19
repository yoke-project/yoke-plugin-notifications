//! notifications: the session's notification server, as a Yoke Plugin unit.

use std::sync::Arc;
use std::time::Duration;

use notifications::backend::{Bus, Said, Server};
use notifications::plugin::{declaration, handle, Notifications, Settings};
use yoke_sdk::plugin::{start, Event};

#[tokio::main]
async fn main() {
    // `notifications --manifest` prints the Manifest the declaration generates, which the bundle carries.
    if std::env::args().nth(1).as_deref() == Some("--manifest") {
        print!("{}", declaration().manifest());
        return;
    }
    let settings = match Settings::parse(std::env::args().skip(1)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("notifications: {e}");
            std::process::exit(2);
        }
    };
    let unit = match start(&declaration()).await {
        Ok(unit) => Arc::new(unit),
        Err(err) => {
            eprintln!("notifications: {err}");
            std::process::exit(1);
        }
    };
    // Registered: now serving on the session's bus. Without it the unit still runs, says why in its health,
    // and answers with an empty list that says the same.
    let notes = Notifications::new(settings.keep);
    let (said_tx, mut said) = tokio::sync::mpsc::unbounded_channel();
    match Server::start(notes.store.clone(), said_tx, Duration::from_millis(settings.transient_ms)).await {
        Ok(server) => notes.serve(server as Arc<dyn Bus>),
        Err(why) => {
            notes.held(false, &why);
            let _ = unit.health(50, &why).await;
        }
    }
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("notifications listens for SIGTERM");
    loop {
        tokio::select! {
            event = unit.next() => match event {
                None | Some(Event::Ended(_)) => return,
                Some(Event::Activated(a)) => {
                    if let Err(err) = notes.activated(unit.as_ref(), &a.stream).await {
                        eprintln!("notifications: {err}");
                    }
                }
                Some(Event::Stopped { stream }) => notes.stopped(&stream),
                Some(event) => {
                    let (unit, notes) = (unit.clone(), notes.clone());
                    tokio::spawn(async move {
                        if let Err(err) = handle(unit.as_ref(), &event, &notes).await {
                            eprintln!("notifications: {err}");
                        }
                    });
                }
            },
            Some(s) = said.recv() => {
                if let Said::Held { held, line } = &s {
                    notes.held(*held, line);
                    if let Err(err) = unit.health(if *held { 0 } else { 50 }, line).await {
                        eprintln!("notifications: {err}");
                    }
                }
                if let Err(err) = notes.said(unit.as_ref(), &s).await {
                    eprintln!("notifications: {err}");
                }
            },
            _ = term.recv() => {
                let _ = unit.close().await;
                while let Some(event) = unit.next().await {
                    if matches!(event, Event::Ended(_)) {
                        break;
                    }
                }
                return;
            }
        }
    }
}
