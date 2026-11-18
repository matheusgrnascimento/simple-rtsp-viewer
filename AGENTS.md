# AGENTS.md

Guidelines for AI agents and contributors working in this repository.

## Project overview

Simple RTSP Viewer is a Windows desktop app that shows multiple RTSP cameras in a grid.

- Language: Rust (edition 2021).
- UI: `egui` / `eframe`.
- Video: FFmpeg 7.x via `ffmpeg-next` (connection, decoding, scaling).
- Async runtime: `tokio`.
- Credentials: Windows Credential Manager via `keyring`. Never store them in files.

### Layout

- `src/main.rs`: app entry point and wiring.
- `src/ui.rs`, `src/dialogs.rs`, `src/theme.rs`, `src/grid.rs`, `src/layout.rs`: interface.
- `src/streaming/`: stream manager, state and frame scaling.
- `src/discovery.rs`: LAN camera discovery.
- `src/credentials.rs`, `src/store.rs`, `src/import.rs`, `src/model.rs`, `src/rtsp_url.rs`: data, persistence and URLs.
- `setup.ps1`: Windows environment setup and release build.

## Language

All content must be written in English: code, identifiers, comments, doc comments, log and error messages, UI text, commit messages, documentation and scripts.

## Build and verify

Requires FFmpeg 7.x shared libraries and LLVM 19 `libclang` (see `README.md` and `setup.ps1`).

```powershell
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Run formatting, clippy and tests before every commit. Fix warnings instead of silencing them.

## Coding conventions

- Follow idiomatic Rust and the existing module structure. Keep modules small and single-purpose.
- No `unwrap()` / `expect()` in non-test code unless the invariant is documented in a comment.
- Never block the UI thread. Do network and decoding work on tokio tasks or worker threads.
- Never log credentials or URLs that contain them. Use the helpers in `rtsp_url.rs` to redact.
- Use `tracing` for logging, not `println!`.
- Add tests for new logic. Use `proptest` for parsing and validation code.
- Do not add dependencies without a clear need. Prefer disabling default features.
- Keep changes scoped. No unrelated refactors in a feature or fix commit.

## Commits

Make atomic commits: each commit contains one logical change, builds, and passes tests on its own.

- Do not mix features, fixes, refactors, formatting and docs in one commit. Split them.
- Commit only the files that belong to the change. Stage deliberately (`git add -p`), not with `git add -A`.
- Use Conventional Commits: `type(scope): summary`.
  - Types: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `chore`, `style`.
  - Summary in imperative mood, lowercase, no trailing period, 72 characters or fewer.
  - Add a body when the reason for the change is not obvious.
- Do not commit build output (`target/`) or local user data (`cameras.json`, `layout.json`).
- Do not rewrite history that has been pushed. Do not force push.

## Security

- Credentials live only in the Windows Credential Manager.
- Treat camera addresses, usernames and passwords as sensitive in logs, errors and test fixtures.
- Network discovery must stay limited to the local subnets of the machine's own interfaces.
