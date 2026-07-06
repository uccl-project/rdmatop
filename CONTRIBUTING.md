# Contributing

## Build & test

rdmatop is Linux-only (netlink + sysfs). CI runs exactly:

```bash
cargo build --release
cargo test
cargo fmt --check
cargo clippy -- -D warnings
```

Hardware-dependent tests are `#[ignore]`d; on a machine with RDMA NICs or
GPUs also run `cargo test -- --ignored`. To verify the TUI under load, use
the traffic generators in `examples/` and cross-check rates against an
independent source (`ethtool -S`, `amd-smi xgmi`, `nvidia-smi nvlink`).

## Ground rules

- Never crash the TUI over missing hardware — sampling degrades to empty
  rows / `None` fields.
- Detect hardware at runtime (`dlopen`), not via cargo features.
- Verify what vendor APIs actually return on real hardware before trusting
  them; leave a short comment citing the observed behavior.
- Comments: 1–3 lines, explain *why*. New code mirrors its neighbors
  (e.g. a new backend follows `src/nvlink.rs` / `src/xgmi.rs`).

## Pull requests

- One logical change per PR; conventional title (`feat:`, `fix:`, `perf:`,
  `docs:`, `refactor:`).
- Fill in the PR template with a real test plan and pasted output. If you
  couldn't test on hardware, say so and explain what you verified instead.
- New behavior needs tests: unit tests for pure logic, an `#[ignore]`d
  smoke test for hardware paths.

## Bug reports

Include: rdmatop version, OS/kernel + driver stack (rdma-core, ROCm, NVIDIA
driver), hardware models, a screenshot or pasted TUI output, and expected vs
actual behavior.
