#![expect(
    clippy::unwrap_used,
    reason = "integration fixtures and assertions fail loudly on setup or command errors"
)]

mod completions;
mod services;
mod workflows;
