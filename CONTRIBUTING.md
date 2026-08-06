# Contributing

Contributions are welcome. Keep the core local-only and deterministic: indexing and routing must not require an LLM, embeddings service, network request, telemetry endpoint, or MCP server.

Before opening a pull request, run:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo package --allow-dirty
```

New behavior should include a focused failure-boundary test. Avoid silently accepting catalog collisions, changed bodies, missing dependencies, path traversal, or partial hydration.
