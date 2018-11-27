//! The Completly Unfair Scheduler

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem;

use process::{ProcessStruct, ThreadStruct, ThreadState};
use i386::process_switch::process_switch;
use sync::{Lock, SpinLockIRQ, SpinLockIRQGuard};
use core::sync::atomic::Ordering;
use error::{UserspaceError};

/// An Arc to the currently running thread.
///
/// In the early kernel initialization, this will be None. Once the first thread takes
/// over, this variable is guaranteed to always be Some and never go back to None - if
/// all threads are currently waiting, the global will point to whatever the last
/// thread was running. A useful side-effect: the CURRENT_PROCESS's pmemory will *always*
/// be the current CR3.
///
/// A side-effect of this guarantee is, if the last thread dies, it will still be kept alive
/// through this global!
///
/// # Safety
///
/// Setting this value should be done through set_current_thread, otherwise Bad Things:tm:
/// will happen.
static mut CURRENT_THREAD: Option<Arc<ThreadStruct>> = None;

/// Gets the current ThreadStruct, incrementing its refcount.
/// Will return None if we're in an early boot state, and it has not yet been initialized.
pub fn try_get_current_thread() -> Option<Arc<ThreadStruct>> {
    unsafe {
        // Safe because modifications only happens in the schedule() function,
        // and outside of that function, seen from a thread' perspective,
        // CURRENT_THREAD will always have the same value
        CURRENT_THREAD.clone()
    }
}

/// Gets the current ThreadStruct, incrementing its refcount.
pub fn get_current_thread() -> Arc<ThreadStruct> {
    try_get_current_thread().unwrap()
}

/// Gets the ProcessStruct of the current thread, incrementing its refcount.
/// Will return None if we're in an early boot state, and it has not yet been initialized.
pub fn try_get_current_process() -> Option<Arc<ProcessStruct>> {
    try_get_current_thread().map(|t| t.process.clone())
}

/// Gets the ProcessStruct of the current thread, incrementing its refcount.
pub fn get_current_process() -> Arc<ProcessStruct> {
    try_get_current_process().unwrap()
}

/// Sets the current ThreadStruct.
///
/// Setting the current thread should *always* go through this function, and never
/// by setting CURRENT_PROCESS directly. This function uses mem::replace to ensure
/// that the ThreadStruct's Drop is run with CURRENT_THREAD set to the *new* value.
///
/// The passed function will be executed after setting the CURRENT_THREAD, but before
/// setting it back to the RUNNING state.
unsafe fn set_current_thread<R, F: FnOnce() -> R>(t: Arc<ThreadStruct>, f: F) -> R {
    mem::replace(&mut CURRENT_THREAD, Some(t.clone()));

    let r = f();

    t.state.compare_and_swap(ThreadState::Scheduled, ThreadState::Running, Ordering::SeqCst);

    r
}

/// The schedule queue
///
/// It's a simple vec, acting as a round-robin, first element is the running thread.
/// When its time slice has ended, it is rotated to the end of the vec, and we go on to the next one.
///
/// The vec is protected by a SpinLockIRQ, so accessing/modifying it disables irqs.
/// Since there's no SMP, this should guarantee we cannot deadlock in the scheduler.
static SCHEDULE_QUEUE: SpinLockIRQ<Vec<Arc<ThreadStruct>>> = SpinLockIRQ::new(Vec::new());

/// Adds a thread at the end of the schedule queue, and changes its state to 'scheduled'
/// Thread must be ready to be scheduled.
///
/// If the thread was already scheduled, this function is a Noop.
///
/// # Panics
///
/// Panics if the thread's state was already "Scheduled"
pub fn add_to_schedule_queue(thread: Arc<ThreadStruct>) {

    let mut queue_lock = SCHEDULE_QUEUE.lock();

    if is_in_schedule_queue(&queue_lock, &thread) {
        return;
    }

    let mut oldstate = thread.state.compare_and_swap(ThreadState::Stopped, ThreadState::Scheduled, Ordering::SeqCst);

    assert!(oldstate == ThreadState::Stopped || oldstate == ThreadState::Killed,
               "Process added to schedule queue was not stopped : {:?}", oldstate);

    queue_lock.push(thread)
}

/// Checks if a thread is already either in the schedule queue or currently running.
pub fn is_in_schedule_queue(queue: &SpinLockIRQGuard<Vec<Arc<ThreadStruct>>>,
                            thread: &Arc<ThreadStruct>) -> bool {
    unsafe { CURRENT_THREAD.iter() }.filter(|v| {
        v.state.load(Ordering::SeqCst) != ThreadState::Stopped
    }).chain(queue.iter()).any(|elem| Arc::ptr_eq(thread, elem))
}

/// Removes the current thread from the schedule queue, and schedule.
///
/// The passed lock will be locked until the thread is safely removed from the schedule queue.
/// In other words, event handling code should wait for that lock to be dropped before attempting
/// to call `add_to_schedule_queue`.
///
/// The reason behind this behavior is that `add_to_schedule_queue` checks if a thread is currently
/// in the schedule queue, before adding it in. It does the check by checking if the thread is
/// either in the list of threads to run, or if it's the currently running one and in a Running
/// state. The lock will be dropped once the thread is transitioned to the Stopped state, allowing
/// `add_to_schedule_queue` to work again.
///
/// It will be relocked just before the thread starts running again. Specifically, it will be
/// relocked when CURRENT_THREAD is set back to the current thread, but before its state is
/// changed back to Running. This allows using SpinLockIRQs as a lock.
///
/// The lock should be used to avoid race conditions between registering for an event, and unscheduling.
///
/// The current thread will not be ran again unless it was registered for rescheduling.
pub fn unschedule<'a, LOCK, GUARD>(lock: &'a LOCK, guard: GUARD) -> Result<GUARD, UserspaceError>
where
    LOCK: Lock<'a, GUARD>,
    GUARD: 'a
{
    {
        let thread = get_current_thread();
        let old = thread.state.compare_and_swap(ThreadState::Running, ThreadState::Stopped, Ordering::SeqCst);
        assert!(old == ThreadState::Killed || old == ThreadState::Running, "Old was in invalid state {:?} before unscheduling", old);
        mem::drop(guard)
    }

    let guard = internal_schedule(lock, true);

    if get_current_thread().state.load(Ordering::SeqCst) == ThreadState::Killed {
        Err(UserspaceError::Canceled)
    } else {
        Ok(guard)
    }
}

/// Creates the very first process at boot.
/// The created process has 1 thread, which is marked as the current thread, and added to the schedule queue.
///
/// # Safety
///
/// Use only for creating the very first process. Should never be used again after that.
/// Must be using a valid KernelStack, a valid ActivePageTables.
///
/// # Panics
///
/// Panics if the schedule queue was not empty
pub unsafe fn create_first_process() {
    let mut queue = SCHEDULE_QUEUE.lock();
    assert!(queue.is_empty());
    let thread_0 = ThreadStruct::create_first_thread();
    unsafe {
        // provided we only run this function once, it hasn't been initialized yet
        set_current_thread(thread_0, || ());
    }
}

/// Performs a process switch.
///
/// # Queue politics
///
///                           checking if thread is unlocked
///                           and suitable for running
///   CURRENT_THREAD          ===============================>
///     j--------j          j--------j j--------j j--------j
///     | current|          |    X   | |        | |        |
///     j--------j          j--------j j--------j j--------j    A
///        | A               locked,       |                    |
///        | |               skipped       |                    |
///        | +-----------------------------+                    |
///        +----------------------------------------------------+
///
/// 1. Tries to lock the next first process. If it fails to acquire its lock,
///    it is ignored for now, and we move on to the next one.
/// 2. When a candidate is found, it is removed from the queue, and
///    set as CURRENT_THREAD.
/// 3. Pushes the previous current thread at the end of the queue.
/// 4. Disables interrupts
/// 5. Performs the process switch
///  * as new process *
/// 6. Re-enables interrupts
pub fn schedule() {
    struct NoopLock;
    impl Lock<'static, ()> for NoopLock {
        fn lock(&self) -> () { () }
    }

    let _ : () = internal_schedule(&NoopLock, false);
}

/// Parses the queue to find the first unlocked process.
/// Returns the index of found process
fn find_next_thread_to_run(queue: &Vec<Arc<ThreadStruct>>) -> Option<usize> {
    for (index, thread) in queue.iter().enumerate() {
        if thread.hwcontext.try_lock().is_some() {
            return Some(index)
        }
    }
    None
}

/// Internal impl of the process switch, used by schedule and unschedule.
///
/// See schedule function for documentation on how scheduling works.
fn internal_schedule<'a, LOCK, GUARD>(lock: &'a LOCK, remove_self: bool) -> GUARD
where
    LOCK: Lock<'a, GUARD>,
    GUARD: 'a
{
    // TODO: Ensure the global counter is <= 1

    let interrupt_manager = SpinLockIRQ::new(());
    let mut interrupt_lock = interrupt_manager.lock();

    loop {
        let mut queue = SCHEDULE_QUEUE.lock();

        let candidate_index = find_next_thread_to_run(&queue);
        let retguard = match (candidate_index, remove_self) {
            (None, true) => {
                // There's nobody to schedule. Let's drop all the locks, HLT, and run internal_schedule again.
                // NOTE: There's nobody running at this point. :O
                drop(queue);
                // Temporarily revive interrupts for hlt.
                drop(interrupt_lock);
                unsafe {
                    ::i386::instructions::interrupts::hlt();
                }

                // Kill interrupts again.
                interrupt_lock = interrupt_manager.lock();

                // Rerun internal_schedule.
                continue;
            },
            (None, false) => {
                // There's nobody else to run. Let's keep running ourselves...
                drop(queue);
                lock.lock()
            }
            (Some(index_b), _) => {
                // 1. remove canditate from the queue, pushing remaining of the queue to the front
                let process_b = queue.remove(index_b);

                // 2. push current at the back of the queue, unless we want to unschedule it.
                let proc = get_current_thread();
                if !remove_self {
                    queue.push(proc.clone());
                }

                // unlock the queue
                drop(queue);

                let whoami = if !Arc::ptr_eq(&process_b, &proc) {
                    unsafe {
                        // safety: interrupts are off
                        process_switch(process_b, proc)
                    }
                } else {
                    // Avoid process switching if we're just rescheduling ourselves.
                    proc
                };

                /* We were scheduled again. To prevent race conditions, relock the lock now. */

                // replace CURRENT_THREAD with ourself.
                // If previously running thread had deleted all other references to itself, this
                // is where its drop actually happens
                unsafe { set_current_thread(whoami.clone(), || lock.lock()) }
            }
        };
        break retguard;
    }
}


/// The function called when a thread was scheduled for the first time,
/// right after the arch-specific process switch was performed.
///
/// It takes a reference to the current thread (which will be set), and a function that should jump
/// to this thread's entrypoint.
///
/// The passed function should take care to change the protection level, and ensure it cleans up all
/// the registers before calling the EIP, in order to avoid leaking information to userspace.
pub fn scheduler_first_schedule<F: FnOnce()>(current_thread: Arc<ThreadStruct>, jump_to_entrypoint: F) {
    // replace CURRENT_THREAD with ourself.
    // If previously running thread had deleted all other references to itself, this
    // is where its drop actually happens
    unsafe { set_current_thread(current_thread, || ()) };

    unsafe {
        // this is a new process, no SpinLockIRQ is held
        ::i386::instructions::interrupts::sti();
    }

    jump_to_entrypoint()
}
