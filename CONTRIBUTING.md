# Contributing

Thank you for helping improve OpenConstructionDesign, a CAD-BIM editor written in Rust. This
document explains how to build and test the project, the rules the code follows, and how
changes get merged.

## Contributor License Agreement

Before your first pull request can be merged you must sign the
[Contributor License Agreement](CLA.md). You keep the copyright in your work; the agreement
lets the copyright holder license your contribution under the AGPL and under the commercial
terms described in [COMMERCIAL.md](COMMERCIAL.md). You assent by signing off on
each commit with `Signed-off-by:` (`git commit -s`, using the email address of your GitHub
account) and, for your first pull request, by commenting "I have read the CLA and I agree"
(section 11 of the agreement).

By participating you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Development setup

1. Install the toolchain described in [docs/BUILDING.md](docs/BUILDING.md): stable Rust 1.89
   or newer with `rustfmt` and `clippy`, and a C compiler (MSVC build tools on Windows).
   Optional: PowerShell 7, Python 3, `cargo-deny`.
2. Fork the repository and clone your fork.
3. Build and run the editor: `cargo run --release -p ddc-app` (Windows). On Linux only the
   library crates and the headless CLI are supported: `cargo build -p ddc-cli`.
4. Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Each crate's `lib.rs` starts with a map
   of its modules; each file starts with a doc comment saying what it owns.

## Code style

- Format with `cargo fmt --all`. Code must pass `cargo clippy --workspace -- -D warnings`.
- No `unsafe` without a clear reason. Every `unsafe` block carries a `// SAFETY:` comment
  explaining why it is sound, and a test that exercises it.
- Errors are typed (`thiserror`); no `unwrap()` or `expect()` outside tests unless the
  invariant is proven locally and stated in a comment.
- Determinism: the same input gives byte-identical output (timestamps excepted).
- Public items are documented. Comments explain *why*, not *what*.
- Keep the dependency direction: `model`, `geom` and `solver` never depend on UI crates;
  importers write to the model only through its public API.
- New dependencies must have a permissive licence compatible with the project licence and
  pass `cargo deny check`.
- User-visible text goes through the localisation catalogs (`crates/ui/data/i18n`), never
  hard-coded strings.
- A new source file starts with an SPDX licence header; `python tools/spdx_headers.py`
  lists files that lack one.
- Source code, comments, documentation and commit messages are in English.
- The model is the source of truth and geometry is derived from it. Every change reaches the
  model as one journaled, undoable operation; never write to the database around the journal.
- The geometry kernel (`ddc-geom`) does not depend on the model.
- Do not use third-party product names or trademarks in code, documentation, UI or commit
  messages. Refer to file formats by extension (`.dwg`, `.rvt`) or by open standard name
  (IFC, glTF, STEP).

## Tests

- **Every rule is a test.** Behaviour that the code promises — a geometric invariant, a
  format rule, a UI contract, a limit — has a test that fails if the promise breaks.
- **Every fixed bug gets a regression test** that fails before the fix.
- Run the tests of the crate you changed, for example `cargo test -q -p ddc-model` or
  `pwsh tools/check.ps1 model` (format, tests and clippy in one command; a test name can
  follow the crate). Run the full workspace (`cargo test -q --workspace`) before you open a
  pull request.
- Changes to the AI assistant must keep all recorded assistant cases passing (the check
  script replays them offline).
- Test fixtures must be created by you or be under a licence that allows redistribution. Do
  not commit proprietary sample files.
- Performance-sensitive changes include before/after numbers from the relevant benchmark.

## Commit messages

```
<area>: <short summary in the imperative mood>

- what changed and why, one point per line
- at most five points
```

`<area>` is a crate or topic: `model`, `geom`, `io`, `render`, `app`, `ai`, `docs`, `ci`, …
Keep the subject under 72 characters. One logical change per commit.

## Issues

Issues are written by users. Maintainers do not open issues on your behalf, so a good report
is the only thing the project has to work with.

- Search existing issues first.
- **Bug reports:** use the bug form. Give the version (`ddc --version`) or commit, the
  platform, steps to reproduce, the expected and the actual result, and a minimal file if
  possible (only files you are allowed to share).
- **Feature requests:** the workflow you are trying to complete and why the current tools do
  not cover it.
- **Security issues are never reported in public issues.** Use GitHub private vulnerability
  reporting as described in [SECURITY.md](SECURITY.md).

## Pull requests

1. Open an issue first for anything larger than a small fix, and agree on the approach.
2. Create a branch from `main`: `feat/<topic>`, `fix/<topic>` or `docs/<topic>`.
3. Keep the pull request focused. Describe what changed, why, and how it was tested.
4. Make sure formatting, clippy, tests and `cargo deny check` pass.
5. At least one maintainer of the affected area reviews the change (see `CODEOWNERS`).
6. Pull requests are squash-merged; the squashed message follows the commit style above.

## Licence

By contributing you agree that your contribution is licensed under the
[AGPL](LICENSE) and the terms of the [CLA](CLA.md).
