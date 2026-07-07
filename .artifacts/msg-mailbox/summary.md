# herdr msg mailbox E2E evidence

Date: 2026-07-07

Session: `msg-e2e`
Binary: `target/release/herdr`

Checks:
- `cargo build --release`: passed after final diff.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo test`: passed. Main binary tests: 1233 passed. Integration suites passed through `server_headless`.

Dogfood flow:
- Started lightweight agent panes `alpha` (`p_1`) and `beta` (`p_2`) in isolated `msg-e2e` session.
- `alpha -> beta` sent message `#1`; send output included `nudged: beta`.
- `beta` inbox returned `#1` and marked it read.
- `beta -> alpha` sent message `#2`; `alpha` inbox returned `#2`.
- With `beta` reported `working`, messages `#3`, `#4`, and `#5` were queued without immediate `nudged`.
- After `beta` reported `idle`, transcript shows one `未読3件 (room=e2e-delay, from=alpha)` nudge.
- `beta` inbox for `e2e-delay` returned `#3`, `#4`, `#5` in id order.

Files:
- `alpha-transcript.txt`
- `beta-transcript.txt`
- `history-e2e.txt`
- `history-e2e-delay.txt`
