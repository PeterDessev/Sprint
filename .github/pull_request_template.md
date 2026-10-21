<!-- What this changes and why. Link the issue it fixes, if any. -->

**Checklist**

- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`
      and `cargo test --workspace` pass
- [ ] Tests, examples and demos use the virtual printer and never open a real
      serial port
- [ ] `ARCHITECTURE.md` is updated if a public item it lists changed
- [ ] `CHANGELOG.md` and `docs/` are updated for user-visible changes
- [ ] Tested on a real printer (say which firmware), or not needed
