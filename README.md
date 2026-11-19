# notifications

A Yoke Plugin that is the desktop's notification server: it holds `org.freedesktop.Notifications` on the
session's bus, so every application's notifications — a mail client, a chat client, a meeting client, the
browser — come to it, and an interface shows them, dismisses them and answers their actions.

| | |
| --- | --- |
| questions | `notifications.list` (the server's state, do-not-disturb, how many are unseen, and every notification held, newest first, with its sequence) |
| commands | `notifications.dismiss` `{id}`, `notifications.dismiss.all` `{app?}`, `notifications.invoke` `{id, action?}`, `notifications.seen` `{}`, `notifications.dnd.set` `{on}` |
| streams | `notifications.list`: the whole record at every change |
| occurrences | `notifications.received` (10, 30 when critical): one came — the application and the urgency, never its text |

Every payload is JSON and states `schema`; a question takes `{}`. A notification carries its `id`, `app`,
`desktop_entry`, `icon`, `image`, `summary`, `body`, `default_action`, `actions` (`{key, label}`),
`urgency` (`low`, `normal`, `critical`), `category`, `at`, `expire_ms` (`null` for the server's choice, 0 for
never), `transient`, `resident`, `seen` and `quiet`.

**A notification is kept until it is dismissed**, closed by its application, or pushed out past `--keep`
(200); the time an application asks for is carried as `expire_ms`, for an interface to time how long it
shows it over the screen. Only one marked `transient` is closed when its time runs out — its own, or
`--transient-ms` (5000). A replacement keeps its id and comes to the top.

**An action is answered as the protocol has it**: `notifications.invoke` tells the application which, and
closes the notification as dismissed unless the application marked it `resident`. `action` defaults to
`default`, the notification's own click.

**Do-not-disturb** keeps every notification and marks those raised under it `quiet`, critical ones apart:
an interface lists them and does not put them over the screen.

**A notification's text is a person's correspondence.** It travels on the stream and in the answer, which
are not kept; the occurrence, which the log store keeps, says only which application and how urgent.

**One server holds the name.** Where another does — mako, dunst, swaync — the Plugin waits in the bus's
queue, says in its health and in the record who holds it, and takes the name the moment that one lets go.
It asks nothing of the Core: a unit is handed no environment of the session, so without
`DBUS_SESSION_BUS_ADDRESS` the account's bus is looked for where systemd puts it, `/run/user/<uid>/bus`.
**A Core run as a service has no person's session**, so no bus of one, and in a container the bus is not
there: the record then says the server does not serve.

Not yet: images sent as data (`image-data`) rather than as a path, and sounds.

Take it into an application with `yoke-compose add plugin https://github.com/yoke-project/yoke-plugin-notifications@<tag>`.
Build it with `yoke-compose build`, and try it from `try/`.

Licensed under the Apache License 2.0.
