# Contributing

Thanks for contributing to ARC Cleaner.

## Development Setup

1. Install Rust (stable) from https://www.rust-lang.org/tools/install.
2. Clone the repo.
3. Copy `.env.example` to `.env` and add your keys.
4. Run:

```bash
cargo run
```

## Before Opening a PR

Run all checks locally:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace --all-targets
```

## Pull Request Guidelines

- Keep PRs focused and small when possible.
- Include a clear description of behavior changes.
- Update docs (`README`, this file, or inline docs) when behavior changes.
- Add or update tests where practical.

## Reporting Issues

Use GitHub Issues and include:

- Expected behavior
- Actual behavior
- Repro steps
- Logs or request IDs for API failures when available
