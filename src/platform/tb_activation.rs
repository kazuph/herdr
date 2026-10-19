//! Defer ordinary CLI termination only while tb's launcher and registry differ.
use std::io;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard};

static PENDING: AtomicI32 = AtomicI32::new(0);
static SERIAL: Mutex<()> = Mutex::new(());

extern "C" fn record(signal: libc::c_int) {
    let _ = PENDING.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
}

pub(crate) struct TbActivationInterrupts {
    old_int: libc::sigaction,
    old_term: libc::sigaction,
    _serial: MutexGuard<'static, ()>,
}

pub(crate) fn defer_tb_activation_interrupts() -> io::Result<TbActivationInterrupts> {
    let serial = SERIAL
        .lock()
        .map_err(|_| io::Error::other("tb signal guard poisoned"))?;
    PENDING.store(0, Ordering::SeqCst);
    // sigaction structures contain no Rust-owned resources; the OS initializes
    // the previous actions and reads the fully initialized replacement below.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    let mut old_int: libc::sigaction = unsafe { std::mem::zeroed() };
    let mut old_term: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = record as *const () as usize;
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGINT, &action, &mut old_int) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::sigaction(libc::SIGTERM, &action, &mut old_term) != 0 {
            let error = io::Error::last_os_error();
            libc::sigaction(libc::SIGINT, &old_int, std::ptr::null_mut());
            return Err(error);
        }
    }
    Ok(TbActivationInterrupts {
        old_int,
        old_term,
        _serial: serial,
    })
}

impl Drop for TbActivationInterrupts {
    fn drop(&mut self) {
        // Activation or its rollback is complete. Restore normal semantics
        // before replaying the first deferred termination signal.
        unsafe {
            libc::sigaction(libc::SIGINT, &self.old_int, std::ptr::null_mut());
            libc::sigaction(libc::SIGTERM, &self.old_term, std::ptr::null_mut());
        }
        let signal = PENDING.swap(0, Ordering::SeqCst);
        if signal != 0 {
            unsafe {
                libc::raise(signal);
            }
        }
    }
}
