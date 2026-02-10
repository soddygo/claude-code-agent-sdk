//! Transport layer for communicating with Claude Code CLI

mod ring_buffer;
pub mod subprocess;
mod trait_def;

pub use subprocess::SubprocessTransport;
pub use trait_def::Transport;
