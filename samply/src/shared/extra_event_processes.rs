//! This module manages "fake" threads for extra perf events like cache-misses, branch-misses, etc.
//!
//! For each (event_name, real_tid) pair, we create a synthetic thread within the same process.
//! The thread is named "{event_name}-{real_thread_name}", e.g., "cache-misses-main".
//! This ensures the samples are properly symbolicated using the real process's library mappings.
//!
//! When `collapse_threads` is enabled, threads are keyed by (event_name, thread_name) instead,
//! so all threads with the same name share a single fake thread per event type.

use std::collections::HashMap;

use fxprof_processed_profile::{ProcessHandle, Profile, ThreadHandle, Timestamp};

/// Tracks fake threads created for extra events within a single process.
#[derive(Debug)]
pub struct ExtraEventThreads {
    /// Maps (event_name, real_tid) -> fake thread handle (used when not collapsing)
    threads_by_tid: HashMap<(String, i32), ThreadHandle>,
    /// Maps (event_name, thread_name) -> fake thread handle (used when collapsing)
    threads_by_name: HashMap<(String, String), ThreadHandle>,
    /// Whether to collapse threads with the same name
    collapse_threads: bool,
}

impl ExtraEventThreads {
    pub fn new(collapse_threads: bool) -> Self {
        Self {
            threads_by_tid: HashMap::new(),
            threads_by_name: HashMap::new(),
            collapse_threads,
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
        let real_name = real_thread_name.unwrap_or("<unknown>");
        let fake_name = format!("{}-{}", event_name, real_name);

        if self.collapse_threads {
            // When collapsing, key by (event_name, thread_name)
            let key = (event_name.to_string(), real_name.to_string());
            *self.threads_by_name.entry(key).or_insert_with(|| {
                // Use a synthetic TID based on the hash of the name
                let fake_tid = -(fake_name.len() as i32 * 1000 + event_name.len() as i32);
                let thread_handle =
                    profile.add_thread(process_handle, fake_tid as u32, start_time, false);
                profile.set_thread_name(thread_handle, &fake_name);
                thread_handle
            })
        } else {
            // When not collapsing, key by (event_name, real_tid)
            let key = (event_name.to_string(), real_tid);
            *self.threads_by_tid.entry(key).or_insert_with(|| {
                // Use a synthetic TID that won't collide with real TIDs
                let fake_tid = -(real_tid.abs().wrapping_mul(1000) + event_name.len() as i32);
                let thread_handle =
                    profile.add_thread(process_handle, fake_tid as u32, start_time, false);
                profile.set_thread_name(thread_handle, &fake_name);
                thread_handle
            })
        }
    }
}
