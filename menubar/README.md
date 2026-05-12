# MenuBar

A macOS menu bar app that shows your Claude Code usage.

## Requirements

macOS 14+, Swift 6.0+. Logged into Claude Code (`claude` CLI).

## Run the app

```bash
./Scripts/package.sh    # build MenuBar.app
open MenuBar.app        # launch
killall MenuBar         # stop
```

The menu bar shows your 5-hour utilization. Click it for the full popover (5h / 7d / Opus / Sonnet bars + reset countdowns).

## Stop Keychain prompts on every rebuild (one-time)

Ad-hoc signed apps get a fresh signature each build, so macOS asks for Keychain access every time. Create a stable dev cert once:

```bash
./Scripts/setup_dev_signing.sh         # generates "MenuBar Dev" cert
# then trust it manually: Keychain Access → cert → Trust → Code Signing → Always Trust
echo 'export APP_IDENTITY="MenuBar Dev"' >> ~/.zshrc
exec $SHELL                            # reload shell
./Scripts/package.sh && open MenuBar.app
# → click "Always Allow" once on the Keychain prompt; never asked again
```

## Run the CLI probe

A debug tool that prints the same usage data to your terminal — useful when something looks off in the GUI.

```bash
swift run ClaudeUsageProbe
```

Output:

```
✓ Loaded credentials from macOS Keychain (Claude Code-credentials)
  scopes: user:profile, ...
  expires: 2026-... (valid)

→ GET https://api.anthropic.com/api/oauth/usage

=== Claude Usage ===
5-hour session: 13.0%  (resets 2026-04-28T...)
7-day total:    19.0%  (resets 2026-05-04T...)
7-day Opus:     n/a
7-day Sonnet:    3.0%  (resets 2026-05-04T...)
```

Common errors:

| Error | Fix |
|---|---|
| `notFound` | Run `claude` in a terminal to log in |
| `401 unauthorized` | Token expired — run `claude` to refresh |
| Keychain prompt | Click "Always Allow" the first time |
