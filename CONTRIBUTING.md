# Contributing

Contributions are welcome. Keep the core local and deterministic. Indexing and routing must not require an LLM, embedding service, network request, telemetry endpoint or MCP server.

Every contribution is subject to the [Contributor Licence Agreement](CONTRIBUTOR-LICENCE-AGREEMENT.md). The agreement lets us publish your work here and use accepted improvements in commercial Codebridge products. Do not submit code you do not have permission to license.

Before opening a pull request, run:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo package --allow-dirty
```

New behaviour should include a focused failure-boundary test. Do not silently accept catalogue collisions, changed bodies, missing dependencies, path traversal or partial hydration.
