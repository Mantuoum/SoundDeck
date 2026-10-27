pub mod decode;
pub mod engine;
pub mod reader;
pub mod resample;
pub mod stream;
pub mod wasapi;

/// Audio workers opt back out of the process-wide below-normal priority, so a
/// background soundboard still gets scheduled in time to keep the device fed.
pub fn raise_thread_priority() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
        };
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
}
