//! Serialisation for the tests that start glassine.
//!
//! The product allows one instance per machine (spec 12), so tests that spawn it
//! must take turns — including tests in another test binary, which `cargo test`
//! may run at the same time and which would otherwise share the log file as well.
//! A named kernel mutex is the primitive the product itself uses, and unlike a
//! lock file the kernel releases it when a test process dies, so a crashed run
//! cannot leave the suite wedged.

#![allow(dead_code)]

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

/// The right to start glassine. Dropping it hands the turn on.
pub struct Turn(HANDLE);

/// Waits for the machine-wide turn.
///
/// Two minutes is far longer than any of these tests take; the timeout exists so
/// that a wedged owner reports itself instead of hanging the suite.
pub fn take_turn() -> Turn {
    // SAFETY: the name is a literal that outlives the call, and both handles are
    // closed in `Turn::drop`.
    unsafe {
        let handle = CreateMutexW(None, false, w!("Glassine.TestAppTurn")).unwrap();
        match WaitForSingleObject(handle, 120_000) {
            // Abandoned means the previous owner died holding it, which is still
            // this test's turn.
            WAIT_OBJECT_0 | WAIT_ABANDONED => Turn(handle),
            WAIT_TIMEOUT => {
                let _ = CloseHandle(handle);
                panic!("another test held the glassine turn for two minutes");
            }
            other => {
                let _ = CloseHandle(handle);
                panic!("waiting for the glassine turn failed: {other:?}");
            }
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateMutexW and is used once more here.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
