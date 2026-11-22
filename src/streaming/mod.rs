//! Stream_Manager: orchestration of the StreamTasks.

mod audio_decode;
mod manager;
mod scale;
mod state;

pub use manager::{StreamChannels, StreamManagerHandle};
