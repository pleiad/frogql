# The wasm the CLI serves

`frogql --serve` compiles these two into the binary, so it works straight
from `cargo install` with no wasm toolchain on the machine. That is the
whole reason they are committed rather than built: `cargo build` cannot
depend on `wasm-pack`, and a `--serve` that prints build instructions
instead of serving is not a feature.

Same discipline as `node/index.js`: generated, committed, and **stale
unless regenerated on every version bump**. Refresh with

```bash
just embed-wasm      # or the two commands it wraps
```

`explorer/pkg/` is the live build output and stays gitignored; `--serve`
prefers it when it exists, so local iteration does not need a refresh
here.
