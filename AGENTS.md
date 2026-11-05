# AGENTS

Guidance for AI coding agents working in this repository.

## Project At A Glance

- Desktop app (Rust + Slint) that syncs local file changes to FTP/FTPS.
- Cargo workspace:
  - `crates/sync-core`: sync engine, no UI dependencies (config, validation, FTP client, watcher/queue, remote paths).
  - `crates/app`: desktop binary `ftp-file-synchronizer` (Slint UI in `ui/app.slint`, glue in `src/main.rs`, single instance in `src/single_instance.rs`).
- Primary documentation: [README.md](README.md).

## Run And Build

- `cargo run -p ftp-file-synchronizer` starts the app.
- `cargo test --workspace` (includes headless UI tests in `crates/app/src/main.rs` using `i-slint-backend-testing`), `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` must pass (CI runs them on Windows, Linux and macOS).
- Packages: `cargo build --release -p ftp-file-synchronizer && cargo packager --release -p ftp-file-synchronizer` (config in `crates/app/Cargo.toml` under `[package.metadata.packager]`).
- Releases are built by `.github/workflows/release.yml` on `v*` tags; never rely on cross-compiling locally.

## Architecture Boundaries

- Keep FTP, config, validation and sync logic in `sync-core`; the app crate only wires UI callbacks to it.
- Long or blocking work (FTP connect, listing, engine start/stop) runs on a background thread and returns to the UI with `upgrade_in_event_loop`. Never block the Slint event loop.
- The engine reports progress only through `EngineEvent` (logs, running state, watcher failure).
- The queue stores changed paths; the action is decided at processing time from the current filesystem state. Do not switch to trusting raw OS event kinds: they differ between platforms.

## Existing Conventions

- Config lives in `<config_dir>/ftp-file-synchronizer/config.json` (format `version: 2`): general preferences plus `profiles`, each with its own connection and projects. The v3.0 single-connection file is migrated into a "Padrão" profile on load.
- Each profile's password goes to the OS keyring under account `ftp-password:<profile id>`, with a plaintext fallback (`plain_passwords`) when no keyring is available (common on Linux). Migration copies the v3.0 `ftp-password` account and never deletes it.
- Ignore patterns use `.gitignore` syntax (`sync_core::ignore_rules`): global patterns first, then the project's. Ignored paths are never uploaded nor removed remotely.
- Disabled projects stay in the config; `validate::config` returns only enabled projects for the engine.
- UI icons are Lucide SVGs in `crates/app/ui/icons` (embedded at build time, tinted with `colorize`). Never use icon fonts or emoji: they render differently per OS.
- Icon-only buttons use the `IconButton` component, whose `tooltip` is also its `accessible-label`; UI tests find buttons and toggles by that label.
- Colors come from the `Style` global in `app.slint` (dark and light tokens, accent gradient). Use its tokens instead of hard-coded colors or `Palette` brushes; Fluent `Palette` colors cannot be overridden, so custom controls (`Toggle`, `Segmented`, `ActionButton`) replace Fluent buttons, switches and combo boxes.
- Toggles never change their own state: `toggled(value)` reports the new value and the owner updates the model/property, so bindings to model data stay intact.
- Profiles live in the sidebar (collapsible; state saved as `sidebar_collapsed`). Collapsed-item tooltips are drawn once at window level because the profile `ListView` clips its children. The active profile's name is the page title (editable). Selecting a profile while Settings is open validates the global ignore patterns and leaves Settings.
- FTP transfers must stay in binary mode (`FileType::Binary`); the library default is ASCII.
- User-facing strings are in Portuguese; keep language consistent unless asked to localize.
- Tests that touch the OS (watcher, FTP server via libunftp) must work on all three platforms; canonicalize temp dirs (macOS `/var` → `/private/var`).

## Pitfalls And Safety Notes

- `sync_core::APP_ID` must match the binary name, the `.desktop` file name, the hicolor icon names and `StartupWMClass`. It is set as the Wayland `app_id`/X11 `WM_CLASS` via `slint::set_xdg_app_id` after `BackendSelector::select()` and before creating the window. Changing any of these breaks Linux icons.
- App icons are generated from `assets/logo.svg` (48 px and up) and `assets/logo-small.svg` (16–32 px) by `assets/render-icons.sh` (needs `rsvg-convert` and ImageMagick). Edit the SVGs and rerun the script; never edit the PNG/ICO files by hand.
- Keep the fixed `fluent` Slint style (`crates/app/build.rs`) so the UI looks the same on every OS.
- Preserve single-instance behavior and native window behavior (minimize to taskbar, close to exit and save config).
- Never mirror deletions when a project's root folder is missing.
- Always stop the engine (drops watchers) before restarting it to avoid duplicate watchers.

## Validation

- Run the commands in "Run And Build".
- After UI or engine changes, run the app and manually check: start/stop sync, test connection, remote folder picker, local folder dialog, profile switch/add/duplicate/delete, project toggle and ignore editor, settings page and theme, log updates, minimize/maximize/close.
- When running the app for manual tests, point `XDG_CONFIG_HOME` (Linux) at a scratch folder to keep the real config untouched. The OS keyring is still the real one.
