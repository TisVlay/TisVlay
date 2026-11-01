## The CLI handles a closed pipe itself, not through SIGPIPE

**Priority**: low
**Found by**: the 2026-11 wake benchmark, where `tisvlay --json list` piped into a missing command panicked

### Context

Rust sets SIGPIPE to ignored before `main`, so a write to a closed pipe returns `EPIPE` instead of killing the process, and `println!` and `eprintln!` panic on that error ([rust-lang/rust#46016](https://github.com/rust-lang/rust/issues/46016), open since 2017). So `tisvlay watch | head`, or a read piped into a reader that had already exited, aborted with a panic message.

The CLI writes with `writeln!` instead. Results are `writeln!(io::stdout(), …)?`, so a broken pipe travels up as an error, and `main` ends that run with status 0, as ripgrep does. Diagnostics are `let _ = writeln!(io::stderr(), …)`: a lost stderr has no reader to tell, and shouldn't stop a pairing or a discovery. The workspace's `print_stdout` and `print_stderr` lints keep the print macros out of the CLI too, and two tests in `tisvlay-cli`'s `workflows.rs` close each stream under a running command.

The usual alternative is to restore SIGPIPE's default disposition at the top of `main`, so the CLI dies the way `cat` does (status 141, which fails a `pipefail` pipeline). It's one call, but it makes the CLI's survival depend on every socket write in its stack suppressing the signal, since the CLI is also a zenoh client over a Unix socket and a write to a socket whose peer has gone raises SIGPIPE too. Today they do: zenoh's Unix-socket link writes through tokio and mio to std's `UnixStream::write`, which sends with `MSG_NOSIGNAL`. std's `write_vectored` on the same socket is a plain `writev` without it, so a dependency moving to vectored writes would make a daemon hanging up mid-rpc kill the CLI with no message. (Checked 2026-11-01: std 1.99.1, tokio 1.54.1, mio 1.2.4, zenoh-link-unixsock_stream 1.10.2.)

Upstream's fix is [rust-lang/rust#150588](https://github.com/rust-lang/rust/issues/150588), a `#[std::io::on_broken_pipe]` item through which a program chooses SIGPIPE's disposition before `main`. As of 2026-11-01 it's unimplemented (the implementation, [rust-lang/rust#150591](https://github.com/rust-lang/rust/pull/150591), is an open PR) and unstable, with open questions about which of its variants stabilize. Its `Kill` and `Inherit` variants are the SIGPIPE reset above, with the same dependency.

### Risk

Low. The lints catch a print macro, but not a stdout write whose error is dropped instead of `?`'d: that command would ignore a reader leaving and keep running, which for `watch` means forever.

### Mitigation options

- **Keep the current handling** (preferred while the stack's socket writes are what they are). It works on stable Rust and doesn't depend on how any dependency writes its sockets.
- **Adopt `on_broken_pipe` once it's stable**, with `Inherit` or `Kill`, and go back to the print macros. Only if every socket write in the CLI's stack still suppresses SIGPIPE, and accepting status 141 under `pipefail`.

### Re-evaluation triggers

- `on_broken_pipe` stabilizes.
- A stdout write found dropping its error, or a command that keeps running after its reader leaves.
