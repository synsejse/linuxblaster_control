# Code conventions

These conventions are shared by razerr-rs and LinuxBlaster Control. Keep this
file and `rustfmt.toml` identical in both repositories.

- Use Rust 2024 formatting, four spaces and a 100-column limit. Separate
  functions, types and implementation blocks with a blank line; keep an item's
  attributes and documentation attached to it.
- Keep CLI argument parsing and process orchestration in binary entry points.
  GUI library entry points expose `run` and `run_demo`, both calling a private
  `launch` function that owns Iced initialization.
- Define `APPLICATION_ID` once in the GUI entry point and pass it to both
  `settings_for` and `window_settings_for`. Keep sizes, fonts, theme and
  subscription setup in the same order. Document lifecycle exceptions such as
  razerr-rs's service-aware close handling.
- Keep `app.rs` for application state and message handling. Keep focused
  helpers such as `activity.rs`, `edit.rs`, `curve.rs` and `worker.rs` or
  `subscriptions.rs` beside it. Use crate-private visibility for cross-module
  helpers; expose only the startup API outside the GUI crate.
- Put page composition and shared page helpers in `view/mod.rs`; each page and
  the sidebar get their own file in `view/`. Keep icon assets and cached sidebar
  handles there. Design components belong in `iced_t3`.
- Keep app behavior tests in `app/tests.rs` and offline render/click tests in
  `render_tests.rs`. Small tests for a helper may remain beside that helper.
- Split by responsibility, not a fixed line count. Comparable responsibilities
  should have matching names and layouts across the apps; differences in
  hardware and process ownership must remain explicit.
- Declare shared dependencies in `[workspace.dependencies]`. Pin Git libraries
  by full `rev` and exact package version, commit `Cargo.lock`, and maintain
  `packaging/cargo-git-hashes.nix` alongside revision changes. Keep reusable
  library tests in their own repositories.

Run `cargo fmt --all -- --check`, workspace Clippy with warnings denied, and
workspace tests before committing. Run private-bus and offline UI tests when
changing their respective paths. Never use hardware setters as smoke tests.
