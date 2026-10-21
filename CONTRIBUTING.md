# Contributing to Sprint

Thanks for helping. Bug reports, fixes, documentation and new features are
all welcome. For anything larger than a fix, open an issue first so we can
agree on the approach before you spend time on it.

## Building

Sprint needs the stable Rust toolchain, 1.95 or newer:

```bash
cargo build
cargo run -- --sim     # the GUI on the virtual printer
```

On Linux, install the libudev development files first (`libudev-dev` on
Debian and Ubuntu, `systemd-devel` on Fedora); the serial-port crate links
against them.

[ARCHITECTURE.md](ARCHITECTURE.md) describes the crates and the contract
between them. Start there.

## Ground rules

* **Never open a real serial port in tests, examples or demos.** Use the
  virtual printer (`sim`, `ConnectTarget::Simulator`) or TCP loopback to
  it. Listing port names is fine; opening them is not, because opening a
  port can reset the board and stop a print.
* **Never touch the user's configuration from tests.** Point
  `SPRINT_CONFIG` at a scratch file or keep the configuration in memory
  (`AppOptions::headless()` in the GUI tests).
* No `unwrap()` on data from files, the network or the printer: malformed
  input must degrade gracefully.
* Safety comes first. Every front end checks typed lines and macros with
  the one command-safety classifier (`sprint_proto::safety::classify`), so
  a line gets the same answer everywhere. A new way to send commands must
  use it too.
* `ARCHITECTURE.md` is a contract. When you change a public item it lists,
  update it in the same commit.

## Before you open a pull request

Run the same checks as CI:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`--workspace` matters: the workspace's default member is only the `sprint`
binary, so a bare `cargo test` skips the other crates.

Some tests are ignored because they are benchmarks or slow in debug builds;
each `#[ignore]` says how to run it.

### GUI snapshots

The GUI tests in `crates/sprint-gui/tests` render with wgpu and compare
the images against `tests/snapshots/*.png`. The committed snapshots were
made on Windows 10 with Segoe UI and Cascadia Mono; on a machine with other
fonts, or without a GPU adapter, the tests still run but skip the image
comparison. When you change how something looks on such a machine,
refresh the snapshots and check the new images before committing them:

```bash
UPDATE_SNAPSHOTS=1 cargo test -p sprint-gui
```

`cargo run -p sprint-gui --example gallery` shows every widget in both
themes, and `cargo run -p sprint-gui --example gui_demo` opens the app on
the virtual printer without touching your configuration.

## Commits

* One logical change per commit, and each commit builds and passes the
  tests.
* [Conventional Commits](https://www.conventionalcommits.org/) with the
  crate as the scope where it fits: `feat(gcode): ...`, `fix(host): ...`,
  `test(sim): ...`, `docs: ...`, `chore: ...`.
* User-visible changes get a line in [CHANGELOG.md](CHANGELOG.md), and
  changed behaviour gets updated docs in `docs/`.

## Reporting bugs

Please include:

* the Sprint version (`sprint --version`) and your operating system;
* the firmware and its version (the `M115` reply, or `firmware` in the
  shell);
* how you connected (the target, such as `COM3@250000` or `sim`);
* what you did, what you expected and what happened, with the console
  output. `RUST_LOG=debug` gives more detail (see
  [Troubleshooting](docs/troubleshooting.md)).

If you can reproduce the problem on the virtual printer, say so: it makes
the bug much easier to fix.

## License

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in Sprint by you, as defined in the Apache-2.0
license, shall be dual licensed under the MIT license and the Apache
License, Version 2.0, without any additional terms or conditions.
