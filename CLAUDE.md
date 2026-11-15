# Zeble

A PostgreSQL browser for macOS (TablePlus/Postico style), written in Rust with GPUI and
[gpui-kit](https://github.com/longbridge/gpui-kit). `README.md` describes the features, the build
dependencies and the release flow; read it first.

- `crates/zeble-core`: everything without UI (connection store, Postgres access, catalog queries,
  SQL splitting, password commands). Put new logic here so it stays testable.
- `crates/zeble-app`: the GPUI app. `workspace.rs` is the connection window, `session.rs` holds the
  per-window connection state, and `grid.rs` is the shared result grid.

## Working with the user

- New features start with a grilling interview (`/grill-me`). Build exactly what was agreed and
  report anything the agreement didn't cover.
- Verify UI changes in the running app (see *Seeing the UI*) and report what you saw, and what you
  could not check.
- Commit finished work to `master` in small commits. The sandbox has no GitHub credentials, so give
  the user the `git push` command.

## Commands

```sh
docker compose up -d                                  # fixture Postgres on localhost:55432 (zeble/zeble)
cargo test --workspace                                # unit + integration tests
cargo clippy --workspace --all-targets -- -D warnings # CI fails on any warning
cargo fmt --all
```

The integration tests in `crates/zeble-core/tests/postgres.rs` **skip silently** when the fixture
database is down. A suspiciously fast run (~0.01 s) means they skipped: start Docker and run them
again. `ZEBLE_TEST_URL` points them at another server. They pass on PostgreSQL 14 and 17.

## Product decisions to keep

- Table browsing is read-only. Cell editing is planned, so keep the data paths ready for it.
  Query tabs run any SQL, unless the connection's read-only switch is on.
- Table data loads in pages (default 500). Header sort cycles ascending → descending → off and
  sorts on the server.
- Query tabs: ⌘↵ runs the selection or the statement at the cursor (tinted), ⌘⇧↵ runs everything.
  Statements run one by one and autocommit. Each tab owns a dedicated connection; browsing shares
  a pool of 4.
- ⌘1–9 select tabs by position, and ⌘W closes the active tab; with no tabs open, ⌘W does nothing.
- Passwords are stored in plain text (`connections.json`, mode 0600), as the user chose. A password
  command's output stays in memory only.
- Releases are signed ad-hoc and not notarized. CI is GitHub Actions. A `v*` tag must match
  `version` in the root `Cargo.toml`.

## Architecture rules

- All database I/O runs on `zeble_core::runtime()`. The UI awaits it with `util::on_db` (or
  `on_db_abortable` when Cancel should kill it). Run pooled work through `Session::with_client` so
  it appears in the top bar and Cancel reaches it.
- Values come back as text through the simple query protocol. Scripts are split client-side by
  `split.rs`; keep that splitter the single place that understands SQL lexing.
- Show errors with `zeble_core::db::error_text`. The `ConnectionLost` and `AuthFailed` context
  markers drive the Disconnected status and the password-command retry.

## GPUI / gpui-kit gotchas

gpui-kit is pinned to `=0.9.1` (GPUI comes from `gpui-pre`). The source of truth for its API is
`~/.cargo/registry/src/*/gpui-component-0.9.1` and `gpui-base-0.9.1`. The GitHub repo's
`crates/story` has usage examples, but its HEAD may have moved on.

- Keystrokes: use `util::keys("mod-x")`, which gives `cmd-` on macOS and `ctrl-` elsewhere. This
  GPUI rejects `secondary-`.
- Updating an entity that is already being updated panics. A `TableDelegate` callback that updates
  its owner must go through `window.defer`, not `cx.defer_in`.
- `use gpui_kit::*` brings in a `test` attribute. In test modules, import names explicitly instead
  of `use super::*`, or write `#[::core::prelude::v1::test]`.
- `Tab::icon` replaces the label; put icons in `.prefix(...)`.
- Only gpui-kit's default icons are embedded. Add any other Lucide icon to `icon_assets!` in
  `assets.rs`, or it renders blank.
- The code editor turns ⌘↵ into a newline. `query_tab.rs` intercepts it with `capture_action` on
  `Enter { secondary: true }`.
- `DataTable` does not take focus on click; `grid::grid_element` focuses it. After switching or
  closing a tab, focus moves into the new tab (`focus_active`), so shortcuts keep reaching the
  window.
- Dialogs and notifications need the window to be opened with `gpui_kit::open_window`, which
  wraps the view in `Root`.

## Seeing the UI (Linux sandbox)

The app runs headless under Xvfb with Mesa's software Vulkan (lavapipe):

```sh
sudo apt-get install -y xvfb mesa-vulkan-drivers imagemagick xdotool   # plus README build deps
Xvfb :99 -screen 0 1440x900x24 &
DISPLAY=:99 ZEBLE_DATA_DIR=<scratch dir> ./target/debug/zeble &      # isolated connections.json
DISPLAY=:99 import -window root shot.png                             # screenshot
DISPLAY=:99 xdotool mousemove 400 213 click 1                        # drive it; keys use ctrl-
```

- The clipboard is blocked in this sandbox, so test copy logic with unit tests.
- `xprop` shows `_NET_WM_ICON` as empty even when it is set; read it with python-xlib.
- macOS builds only work on a Mac: `scripts/bundle-macos.sh` locally, `release.yml` in CI. Here you
  can type-check macOS-only code with a throwaway crate and `cargo check --target aarch64-apple-darwin`.
- After changing `icon.svg`, run `scripts/render-icon.sh` and commit `assets/icon/`.
