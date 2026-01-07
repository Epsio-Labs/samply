//! This module manages "fake" threads for extra perf events like cache-misses, branch-misses, etc.
//!
//! For each (event_name, real_tid) pair, we create a synthetic thread within the same process.
//! The thread is named "{event_name}-{real_thread_name}", e.g., "cache-misses-main".
//! This ensures the samples are properly symbolicated using the real process's library mappings.

use std::collections::HashMap;

use fxprof_processed_profile::{ProcessHandle, Profile, ThreadHandle, Timestamp};

/// Tracks fake threads created for extra events within a single process.
/// Maps (event_name, real_tid) -> fake ThreadHandle
#[derive(Debug, Default)]
pub struct ExtraEventThreads {
    /// Maps (event_name, real_tid) -> fake thread handle
    threads: HashMap<(String, i32), ThreadHandle>,
}

impl ExtraEventThreads {
    pub fn new() -> Self {
        Self {
            threads: HashMap::new(),
        }
    }

    /// Get or create a fake thread for the given event and real thread.
    /// The fake thread is created in the same process and named "{event_name}-{real_thread_name}".
    pub fn get_or_create_thread(
        &mut self,
        event_name: &str,
        real_tid: i32,
        real_thread_name: Option<&str>,
        process_handle: ProcessHandle,
        start_time: Timestamp,
        profile: &mut Profile,
    ) -> ThreadHandle {
        let key = (event_name.to_string(), real_tid);

        *self.threads.entry(key).or_insert_with(|| {
            let real_name = real_thread_name.unwrap_or("<unknown>");
            let fake_name = format!("{}-{}", event_name, real_name);
            // Use a synthetic TID that won't collide with real TIDs
            let fake_tid = -(real_tid.abs().wrapping_mul(1000) + event_name.len() as i32);
            let thread_handle =
                profile.add_thread(process_handle, fake_tid as u32, start_time, false);
            profile.set_thread_name(thread_handle, &fake_name);
            thread_handle
        })
    }

    /// Check if a thread exists for the given event and real_tid.
    #[allow(dead_code)]
    pub fn has_thread(&self, event_name: &str, real_tid: i32) -> bool {
        self.threads
            .contains_key(&(event_name.to_string(), real_tid))
    }
}
