# Trying notifications

This directory is an application project that uses the Plugin in `..`: `yoke-compose init` made it and
`add plugin .. --yes` took the Plugin into it. Nothing here is the Plugin, and taking the Plugin into
another application never takes this directory.

| Path | What it is |
| --- | --- |
| `yoke.yaml` | the application: the unit `notifications` and the channel `panel`, a local socket for one client |
| `follow/` | an interface client: it asks every question, follows every stream and every event of the unit, and prints each as one line |
| `run.sh` | runs the application on a local Core, grants what the Plugin declares, and follows the unit |

A notification centre for it is a shell's: `yk-qshell`'s speaks to this Plugin through the contract.

The programs of Yoke are installed from the module proxy, the newest release or a tag:

```sh
for p in yoke yoke-compose yoke-core yoke-guardian yoke-auth; do
  go install "github.com/yoke-project/yoke/cmd/$p@latest"
done
```

```sh
./run.sh                    # one line per answer, frame and event, until interrupted
./run.sh --read-only        # the question, the stream and the occurrence, and no command
./run.sh --seconds 10
```

Where another server holds the name, the record says which and the Plugin waits: stop it (`pkill dunst`,
or whichever your desktop runs) to see the Plugin take the name. Then raise one from another
terminal with `notify-send -A ok=OK "Hello" "from notify-send"`, and answer it, naming the instance
`run.sh` prints:

    echo '{"id":1,"action":"ok"}' | yoke unit command notifications notifications.invoke --input - --instance <instance>
