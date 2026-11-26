# Building and testing

## Toolchain

- **Rust:** current stable via `rustup`, edition 2021, with `rustfmt` and `clippy`
  (`rustup component add rustfmt clippy`).
- **C compiler:** required by bundled C sources (SQLite, the Lua interpreter). On Windows,
  the MSVC build tools; on other systems, a system C compiler (`cc`/`clang`).
- **Linker on Windows:** `.cargo/config.toml` selects `rust-lld.exe` for
  `x86_64-pc-windows-msvc`; it ships with the Rust toolchain.
- **Optional:** PowerShell 7 (`pwsh`) for `tools/check.ps1`; Python 3 for the helper scripts
  in `tools/`; `cargo-deny` for licence and advisory checks.

No system libraries are needed for SQLite or Lua: both are built from bundled sources.

## Platforms

| Platform | Status |
|---|---|
| Windows x86_64 | Supported; primary development and test platform. |
| macOS | Platform code paths exist (graphics backend selection, key storage); not regularly built or tested. |
| Linux | Library crates and the headless `ddc` CLI. The editor's window layer is configured without a Linux windowing backend (`x11`/`wayland`), so the GUI is not supported on Linux yet. API keys are kept in memory only (no OS key store integration). |
| Browser (wasm32) | The full editor as a WebAssembly bundle (`crates/web-editor`, `tools/web`); built and checked in CI. See [BROWSER.md](BROWSER.md). |

The renderer uses wgpu: Direct3D 12, Vulkan or OpenGL on Windows, Metal on macOS. `--safe-graphics`
starts the editor with conservative graphics settings.

## Commands

```sh
# Editor (debug build is slow for large models; prefer --release)
cargo build --release -p ddc-app
cargo run   --release -p ddc-app

# Headless CLI, no window or GPU
cargo build --release -p ddc-cli
cargo run   --release -p ddc-cli -- info model.ifc

# Windowless editor run on a file: import, export, JSON report
cargo run --release -p ddc-app -- --file model.ifc --export model.glb

# Optional features
cargo build --release -p ddc-app --features link     # project server link
cargo build --release -p ddc-cli --features link     # `ddc gen`, `ddc publish`
```

Binaries are written to `target/release/` (`ddc-app`, `ddc`). Settings, scripts and
installed bridges live in the user configuration directory (`%APPDATA%\ddc-editor` on
Windows, `$XDG_CONFIG_HOME/ddc-editor` or `~/.config/ddc-editor` elsewhere). `DDC_BRIDGE_DIR`
overrides where bridge packages are looked up.

## Tests

`tools/check.ps1` runs formatting, tests and clippy for one crate or the workspace and prints
only results and errors:

```powershell
pwsh tools/check.ps1                  # whole workspace
pwsh tools/check.ps1 model            # crate ddc-model
pwsh tools/check.ps1 geom sweep       # crate ddc-geom, tests matching "sweep"
pwsh tools/check.ps1 assets           # asset checks (Python)
```

When the change touches `crates/ai` or `crates/app/src/ai`, the script also replays the
recorded assistant cases offline (no network); they must all pass.

Cargo equivalents:

```sh
cargo test -q -p ddc-model                 # one crate
cargo test -q -p ddc-geom sweep            # one crate, filtered
cargo test -q --workspace                  # everything (slow)
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
cargo deny check                           # licences, advisories, duplicates (deny.toml)
```

Test only the crate you changed; run the full workspace before opening a pull request.
`cargo test -p ddc-app --features test-hooks` enables the model's SQL statement counter used
by frame-budget tests.
