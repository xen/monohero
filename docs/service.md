# Running the service

Start `monohero serve` explicitly in a terminal for initial use. One service owns
one state directory. It sleeps on socket/condition-variable events when idle.
For unattended use, choose one of these user-level service examples. Replace all
absolute paths with your own. Do not run as root. Service setup is opt-in.

## Linux: systemd user service

Create `~/.config/systemd/user/monohero.service`:

```ini
[Unit]
Description=Monohero local workflow queues

[Service]
ExecStart=/home/YOU/.cargo/bin/monohero serve
Environment=PATH=/home/YOU/.cargo/bin:/usr/local/bin:/usr/bin:/bin
Restart=no
KillMode=control-group

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now monohero
journalctl --user -u monohero
```

`Restart=no` deliberately exposes crashes and surviving jobs. A service stop kills
its process group; inspect interrupted jobs before acknowledging them after restart.
User services normally follow user login lifetime. Configure lingering separately
only if you explicitly want execution after logout.

## macOS: launchd agent

Create `~/Library/LaunchAgents/local.monohero.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>local.monohero</string>
  <key>ProgramArguments</key><array>
    <string>/Users/YOU/.cargo/bin/monohero</string><string>serve</string>
  </array>
  <key>EnvironmentVariables</key><dict>
    <key>PATH</key><string>/Users/YOU/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>StandardErrorPath</key><string>/Users/YOU/Library/Logs/monohero.log</string>
</dict></plist>
```

```sh
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/local.monohero.plist
```

No KeepAlive restart loop. RunAtLoad starts it on login. Inspect service logs and
surviving children before restarting after a crash. Terminal shell configuration
is not loaded by user services; configure PATH and workflow credentials explicitly.

For removal, disable/stop the service using your service manager, then delete its
configuration. Stopping the service can interrupt jobs. Keep the state directory
until logs, external effects and pending jobs have been inspected.
