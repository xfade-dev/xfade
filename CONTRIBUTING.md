# Contributing

Thanks for your interest in contributing to xfade!

## Getting started

- Rust (stable) is required for the core library and CLI. Node.js (20+) is required for the GUI frontend.
- Run the test suite: `cargo test --workspace`
- Run linting: `cargo clippy --all-targets --workspace -- -D warnings`
- Formatting: `cargo fmt --all`

## Development flow

1. Open an issue to discuss what you'd like to change (for anything non-trivial).
2. Fork the repo and create a branch.
3. Make your changes, adding tests where it makes sense.
4. Ensure `cargo test --workspace` and `cargo clippy --all-targets --workspace -- -D warnings` pass.
5. Open a pull request describing the change and linking the issue.

## Code style

- Follow `cargo fmt` (rustfmt) and clippy with `-D warnings`.
- Identifiers and code comments are in English.
- Keep the CLI output and GUI text in English (see `docs/design-decisions.md`).

## Project layout

See [docs/architecture.md](docs/architecture.md) for the workspace structure and data flow.

## License

By contributing, you agree that your contributions are licensed under the same terms as the project (MIT OR Apache-2.0).
