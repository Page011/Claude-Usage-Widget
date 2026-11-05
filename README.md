# Claude usage widget

A small window in the corner of your screen that shows how much of your Claude usage you've used. It shows the 5-hour limit and the weekly limit.

It runs on Windows. Written in Rust.

## Using it

- Click it to open or close the graphs.
- Drag it to move it.
- Right-click for the menu.
- The tray icon (bottom right) has one thing: Quit. It closes every widget.

The right-click menu has:

- **Refresh now**
- **Reset position**
- **Log in to Claude** (only shows when you need to sign in again)
- **+ Widget** (adds another one, for a second screen)
- **Remove Widget** (or **End Task** if it's the last one)

## Where the numbers come from

Claude Code saves a sign-in token on your PC. The widget uses that token to ask Claude for your usage. You get the same numbers `/usage` shows.

- It checks every 5 minutes.
- The token only goes to Anthropic. The widget never writes it to a file or logs it.
- If the token has run out, the widget says so. Right-click and choose **Log in to Claude**, then sign in in your browser. Using Claude Code also refreshes the token.
- Anthropic doesn't document this usage page, so it could change. If the format changes, the widget says `API CHANGED` instead of showing a wrong number.

## Colours and warnings

- **Blue**: fine.
- **Amber**: at this pace you'll hit the limit before it resets.
- **Red**: 90% or more.

A `!` after the number shows the same warning, so you don't have to rely on colour.

The graph shows your usage so far, plus a dashed line for where you'll end up at this pace.

## Files

These are saved in `%APPDATA%\usage-widget\`:

- `history.csv`: usage samples. Kept for 8 days.
- `config.json`: where the widgets sit for each screen setup.
- `widget.log`: start-up messages and errors. Cleared when it passes 512 KB.
- `login.log`: output from the last **Log in to Claude** attempt.

## Building

GitHub builds it for you. Every push runs the tests and builds the exe. You'll find it in the **Actions** tab.

To publish a release, push a tag like `v0.2.0`. The exe then goes on the **Releases** page.

The exe isn't signed, so Windows SmartScreen will warn you the first time you run it.

To run the checks on your own PC:

```
cargo test
cargo clippy --all-targets
```

## Known issues

- A widget in the top-right corner can cover the close button of a maximised window. Drag it somewhere else.
