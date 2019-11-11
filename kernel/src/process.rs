//! Process

use crate::stack::KernelStack;
use crate::i386::process_switch::*;
use crate::paging::process_memory::ProcessMemory;
use alloc::sync::{Arc, Weak};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use crate::event::{IRQEvent, ReadableEvent, WritableEvent, Waitable};
use crate::sync::{SpinLockIRQ, SpinLock, Mutex};
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::scheduler;
use crate::error::{KernelError, UserspaceError};
use crate::ipc::{ServerPort, ClientPort, ServerSession, ClientSession};
use crate::mem::VirtualAddress;
use failure::Backtrace;
use crate::frame_allocator::PhysicalMemRegion;
use crate::sync::SpinRwLock;

use atomic::Atomic;

pub mod thread_local_storage;
mod capabilities;
pub use self::capabilities::ProcessCapabilities;
use crate::paging::{InactiveHierarchy, InactiveHierarchyTrait, PAGE_SIZE, MappingAccessRights};
use self::thread_local_storage::TLSManager;
use crate::i386::interrupt_service_routines::UserspaceHardwareContext;
use sunrise_libkern::process::{ProcessState, ProcInfo};
use sunrise_libkern::MemoryType;

/// List of processes currently running on the system.
pub static PROCESS_LIST: Mutex<Vec<Weak<ProcessStruct>>> = Mutex::new(Vec::new());

/// Data related to the (user-visible) state the current process is in. The
/// maternity is stored here to ensure there is no race condition between
/// setting the state to Exited and adding threads to the maternity.
#[derive(Debug)]
struct ProcessStateData {
    /// Whether the process is currently in a signaled state. Set to true when
    /// changing the state, and false when the user calls reset_signal(). Note
    /// that once the Exited state is reached, the process is permanently
    /// signaled.
    signaled: bool,

    /// The current state of the process.
    state: ProcessState,

    /// Threads waiting on this process to get signaled.
    waiting_threads: Vec<Arc<ThreadStruct>>,

    /// An array of the created but not yet started threads.
    ///
    /// When we create a thread, we return a handle to userspace containing a weak reference to the thread,
    /// which has not been added to the schedule queue yet.
    /// To prevent it from being dropped, we must keep at least 1 strong reference somewhere.
    /// This is the job of the maternity. It holds references to threads that no one has for now.
    /// When a thread is started, its only strong reference is removed from the maternity, and put
    /// in the scheduler, which is now in charge of keeping it alive (or not).
    ///
    /// Note that we store in the process struct a strong reference to a thread, which itself
    /// has a strong reference to the same process struct. This creates a cycle, and we loose
    /// the behaviour of "a process is dropped when its last *living* thread is dropped".
    /// The non-started threads will keep the process struct alive. This makes it possible to
    /// pass a non-started thread handle to another process, and let it start it for us, even after
    /// our last living thread has died.
    ///
    /// However, because of this, if a thread creates other threads, does not share the handles,
    /// and dies before starting them, the process struct will be kept alive indefinitely
    /// by those non-started threads that no one can start, and the process will stay that way
    /// until it is explicitly stopped from outside.
    // TODO: Use a better lock around thread_maternity.
    // BODY: Thread maternity currently uses a SpinLock. We should ideally use a
    // BODY: scheduling mutex there.
    thread_maternity: Vec<Arc<ThreadStruct>>,
}

impl ProcessStateData {
    /// Sets the state to the given new state, and signal the process, causing
    /// any threads waiting on the process to get woken up.
    fn set_state(&mut self, newstate: ProcessState) {
        self.state = newstate;
        self.signal();
    }

    /// Set the process to the signaled state, and wake up any thread waiting
    /// on this process to get signaled.
    fn signal(&mut self) {
        self.signaled = true;
        while let Some(thread) = self.waiting_threads.pop() {
            scheduler::add_to_schedule_queue(thread);
        }
    }
}


/// The struct representing a process. There's one for every process.
///
/// It contains many information about the process :
///
/// - Its type (regular userspace process, or kworker)
/// - Its state
/// - Its memory pages
/// - Its kernel stack, for syscalls and interrupts
/// - Its hardware context, to be restored on rescheduling
#[derive(Debug)]
pub struct ProcessStruct {
    /// The unique id of this process.
    pub pid:                  usize,
    /// A name for this process.
    pub name:                 String,
    /// The memory view of this process. Shared among the threads.
    pub pmemory:              Mutex<ProcessMemory>,
    /// The handles of this process. Shared among the threads.
    pub phandles:             SpinLockIRQ<HandleTable>,
    /// The threads of this process.
    /// A ProcessStruct with no thread left will eventually be dropped.
    // todo choose a better lock
    pub threads:              SpinLockIRQ<Vec<Weak<ThreadStruct>>>,

    /// The entrypoint of the main thread.
    pub entrypoint:           VirtualAddress,

    /// Permissions of this process.
    pub capabilities:             ProcessCapabilities,

    /// The state the process is currently in.
    state:                    Mutex<ProcessStateData>,

    /// Tracks used and free allocated Thread Local Storage regions of this process.
    pub tls_manager: Mutex<TLSManager>,
}

/// Next available PID.
///
/// PIDs are just allocated sequentially in ascending order, and reaching usize::max_value() causes a panic.
static NEXT_PROCESS_ID: AtomicUsize = AtomicUsize::new(0);

/// The struct representing a thread. A process may own multiple threads.
#[derive(Debug)]
pub struct ThreadStruct {
    /// The state of this thread.
    pub state: Atomic<ThreadState>,

    /// The kernel stack it uses for handling syscalls/irqs.
    pub kstack: KernelStack,

    /// The saved hardware context, for getting it running again on a process_switch.
    pub hwcontext: SpinLockIRQ<ThreadHardwareContext>,

    /// The process that this thread belongs to.
    ///
    /// # Description
    ///
    /// A process has a link to its threads, and every thread has a link back to its process.
    /// The thread owns a strong reference to the process it belongs to, and the process in turn
    /// has a vec of weak references to the threads it owns.
    /// This way dropping a process is done automatically when its last thread is dropped.
    ///
    /// The currently running process is indirectly kept alive by the `CURRENT_THREAD` global in scheduler.
    pub process: Arc<ProcessStruct>,

    /// Pointer to the Thread Local Storage region of this thread.
    ///
    /// * x86_32: loaded in the `fs` segment selector.
    /// * x86_64: loaded in the `gs` segment selector.
    pub tls_region: VirtualAddress,

    /// Userspace's elf `Thread Pointer`.
    ///
    /// * x86_32: loaded in the `gs` segment selectors.
    /// * x86_64: loaded in the `fs` segment selectors.
    pub tls_elf: SpinLock<VirtualAddress>,

    /// Userspace hardware context of this thread.
    ///
    /// Registers are backed up every time we enter the kernel via a syscall/exception, for debug purposes.
    pub userspace_hwcontext: SpinLockIRQ<UserspaceHardwareContext>,

    /// Thread state event
    ///
    /// This is used when signaling that this thread as exited.
    state_event: ThreadStateEvent
}

/// A handle to a userspace-accessible resource.
///
/// # Description
///
/// When the userspace manipulates a kernel construct, it does so by operating
/// on Handles, which are analogious to File Descriptor in a Unix System. A
/// Handle represents all kinds of kernel structures, from IPC objects to
/// Threads and IRQ events.
///
/// A Handle may be shared across multiple processes, usually by passing it via
/// IPC. This can be used, for instance, to share a handle to a memory region,
/// allowing for the mapping of Shared Memory.
///
/// Most handles can be waited on via [crate::syscalls::wait_synchronization], which
/// will have relevant behavior for all the different kind of handles.
#[derive(Debug)]
pub enum Handle {
    /// A special ReadableEvent that is triggered automatically when an IRQ is
    /// triggered.
    InterruptEvent(IRQEvent),
    /// An event on which we can wait, triggered by a WritableEvent.
    ReadableEvent(ReadableEvent),
    /// Trigger for an associated ReadableEvent.
    WritableEvent(WritableEvent),
    /// The server side of an IPC port. See [crate::ipc::port] for more information.
    ServerPort(ServerPort),
    /// The client side of an IPC port. See [crate::ipc::port] for more information.
    ClientPort(ClientPort),
    /// The server side of an IPC session. See [crate::ipc::session] for more
    /// information.
    ServerSession(ServerSession),
    /// The client side of an IPC session. See [crate::ipc::session] for more
    /// information.
    ClientSession(ClientSession),
    /// A thread.
    Thread(Weak<ThreadStruct>),
    /// A process.
    Process(Arc<ProcessStruct>),
    /// A shared memory region. The handle holds on to the underlying physical
    /// memory, which means the memory will only get freed once all handles to
    /// it are dropped.
    SharedMemory(Arc<SpinRwLock<Vec<PhysicalMemRegion>>>),
}

/// The underlying shared object of a [Weak<ThreadStrct>].
#[derive(Debug)]
struct ThreadStateEvent {
    /// List of threads waiting on this thread to exit. When this thread exit, all
    /// those threads will be rescheduled.
    waiting_threads: SpinLock<Vec<Arc<ThreadStruct>>>
}

impl ThreadStateEvent {
    /// Signals the event, waking up any thread waiting on its value.
    pub fn signal(&self) {
        let mut threads = self.waiting_threads.lock();
        while let Some(thread) = threads.pop() {
            scheduler::add_to_schedule_queue(thread);
        }
    }
}

/// If this waitable is signaled, this means the thread has exited.
impl Waitable for Weak<ThreadStruct> {
    fn is_signaled(&self) -> bool {
        if let Some(thread) = self.upgrade() {
            return thread.state.load(Ordering::SeqCst) == ThreadState::TerminationPending;
        }

        // Cannot upgrade to Arc? The thread is dead, so it totally have exited!
        true
    }

    fn register(&self) {
        if let Some(thread) = self.upgrade() {
            thread.state_event.waiting_threads.lock().push(scheduler::get_current_thread());
        }
    }
}

impl Handle {
    /// Gets the handle as a [Waitable], or return a `UserspaceError` if the handle cannot be waited on.
    pub fn as_waitable(&self) -> Result<&dyn Waitable, UserspaceError> {
        match *self {
            Handle::ReadableEvent(ref waitable) => Ok(waitable),
            Handle::InterruptEvent(ref waitable) => Ok(waitable),
            Handle::ServerPort(ref serverport) => Ok(serverport),
            Handle::ServerSession(ref serversession) => Ok(serversession),
            Handle::Thread(ref thread) => Ok(thread),
            Handle::Process(ref process) => Ok(process),
            _ => Err(UserspaceError::InvalidHandle),
        }
    }

    /// Casts the handle as a [ClientPort], or returns a `UserspaceError`.
    pub fn as_client_port(&self) -> Result<ClientPort, UserspaceError> {
        if let Handle::ClientPort(ref s) = *self {
            Ok((*s).clone())
        } else {
            Err(UserspaceError::InvalidHandle)
        }
    }

    /// Casts the handle as a [ServerSession], or returns a `UserspaceError`.
    pub fn as_server_session(&self) -> Result<ServerSession, UserspaceError> {
        if let Handle::ServerSession(ref s) = *self {
            Ok((*s).clone())
        } else {
            Err(UserspaceError::InvalidHandle)
        }
    }

    /// Casts the handle as a [ClientSession], or returns a `UserspaceError`.
    pub fn as_client_session(&self) -> Result<ClientSession, UserspaceError> {
        if let Handle::ClientSession(ref s) = *self {
            Ok((*s).clone())
        } else {
            Err(UserspaceError::InvalidHandle)
        }
    }

    /// Casts the handle as a Weak<[ThreadStruct]>, or returns a `UserspaceError`.
    pub fn as_thread_handle(&self) -> Result<Weak<ThreadStruct>, UserspaceError> {
        if let Handle::Thread(ref s) = *self {
            Ok((*s).clone())
        } else {
            Err(UserspaceError::InvalidHandle)
        }
    }

    /// Casts the handle as an Arc<[ProcessStruct]>, or returns a `UserspaceError`.
    pub fn as_process(&self) -> Result<Arc<ProcessStruct>, UserspaceError> {
        if let Handle::Process(ref s) = *self {
            Ok((*s).clone())
        } else {
            Err(UserspaceError::InvalidHandle)
        }
    }

    /// Casts the handle as an Arc<[WritableEvent]> if the handle is a
    /// [WritableEvent], or returns a `UserspaceError`.
    pub fn as_writable_event(&self) -> Result<WritableEvent, UserspaceError> {
        match self {
            Handle::WritableEvent(event) => Ok(event.clone()),
            _ => Err(UserspaceError::InvalidHandle)
        }
    }

    /// Casts the handle as an Arc<[ReadableEvent]>, or returns a `UserspaceError`.
    pub fn as_readable_event(&self) -> Result<ReadableEvent, UserspaceError> {
        match self {
            Handle::ReadableEvent(event) => Ok(event.clone()),
            _ => Err(UserspaceError::InvalidHandle)
        }
    }

    /// Casts the handle as an Arc<SpinRwLock<Vec<[PhysicalMemRegion]>>>, or returns a
    /// `UserspaceError`.
    pub fn as_shared_memory(&self) -> Result<Arc<SpinRwLock<Vec<PhysicalMemRegion>>>, UserspaceError> {
        if let Handle::SharedMemory(ref s) = *self {
            Ok((*s).clone())
        } else {
            Err(UserspaceError::InvalidHandle)
        }
    }
}

/// Holds the table associating userspace handle numbers to a kernel [Handle].
///
/// Each process holds a table associating a number to a kernel Handle. This
/// number is unique to that process, handles are not shared between processes.
///
/// Handle numbers hold two guarantees.
///
/// - It will not be reused ever. If a userspace attempts to use a handle after
///   closing it, it will be guaranteed to receive an InvalidHandle error.
/// - It will always be above 0, and under 0xFFFF0000.
///
/// Technically, a Horizon/NX handle is composed of two parts: The top 16 bits
/// are randomized, while the top 16 bits are an auto-incrementing counters.
/// Because of this, it is impossible to have more than 65535 handles.
///
/// In Sunrise, we do not yet have randomness, so the counter just starts from 1 and
/// goes up.
///
/// There exists two "meta-handles": 0xFFFF8000 and 0xFFFF8001, which always
/// point to the current process and thread, respectively. Those handles are not
/// *actually* stored in the handle table to avoid creating a reference cycle.
/// Instead, they are retrieved dynamically at runtime by the get_handle
/// function.
#[derive(Debug)]
pub struct HandleTable {
    /// Internal mapping from a handle number to a Kernel Object.
    table: BTreeMap<u32, Arc<Handle>>,
    /// The next handle's ID.
    counter: u32
}

impl Default for HandleTable {
    /// Creates an empty handle table. Note that an empty handle table still
    /// implicitly contains the meta-handles 0xFFFF8000 and 0xFFFF8001.
    fn default() -> Self {
        HandleTable {
            table: BTreeMap::new(),
            counter: 1
        }
    }
}

impl HandleTable {
    // TODO: HandleTable::add_handle should error if the table is full.
    // BODY: HandleTable::add_handle currently assumes that insertion will always
    // BODY: succeed. It does not implement any handle count limitations present
    // BODY: in Horizon/NX. Furthermore, if the handle table is completely filled
    // BODY: (e.g. there are 2^32 handles), the function will infinite loop.
    // BODY: And as if that wasn't enough: it doesn't technically guarantee a
    // BODY: handle will not get reused.
    /// Add a handle to the handle table, returning the userspace handle number
    /// associated to the given handle.
    #[allow(clippy::map_entry)]
    pub fn add_handle(&mut self, handle: Arc<Handle>) -> u32 {
        loop {
            let handlenum = self.counter;
            self.counter += 1;
            if !self.table.contains_key(&handlenum) {
                self.table.insert(handlenum, handle);
                break handlenum;
            }
        }
    }

    /// Gets the Kernel Handle associated with the given userspace handle number.
    ///
    /// # Errors
    ///
    /// - `InvalidHandle`
    ///    - The provided handle does not exist in the handle table.
    pub fn get_handle(&self, handle: u32) -> Result<Arc<Handle>, UserspaceError> {
        match handle {
            0xFFFF8000 => Ok(Arc::new(Handle::Thread(Arc::downgrade(&scheduler::get_current_thread())))),
            0xFFFF8001 => Ok(Arc::new(Handle::Process(scheduler::get_current_process()))),
            handle => self.table.get(&handle).cloned().ok_or(UserspaceError::InvalidHandle)
        }
    }

    /// Gets the Kernel Handle associated with the given userspace handle number.
    ///
    /// Does not interpret 0xFFFF8000 and 0xFFFF8001.
    ///
    /// # Errors
    ///
    /// - `InvalidHandle`
    ///    - The provided handle does not exist in the handle table.
    pub fn get_handle_no_alias(&self, handle: u32) -> Result<Arc<Handle>, UserspaceError> {
        self.table.get(&handle).cloned().ok_or(UserspaceError::InvalidHandle)
    }

    /// Deletes the mapping from the given userspace handle number. Returns the
    /// underlying Kernel Handle, in case it needs to be used (e.g. for sending
    /// to another process in an IPC move).
    pub fn delete_handle(&mut self, handle: u32) -> Result<Arc<Handle>, UserspaceError> {
        // TODO: Handle 0xFFFF8000 and 0xFFFF8001 ?
        self.table.remove(&handle).ok_or(UserspaceError::InvalidHandle)
    }
}

/// The state of a thread.
///
/// - Paused: not in the scheduled queue, waiting for an event
/// - Running: currently on the CPU
/// - TerminationPending: dying, will be unscheduled and dropped at syscall boundary
/// - Scheduled: scheduled to be running
///
/// Since SMP is not supported, there is only one Running thread.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum ThreadState {
    /// Not in the scheduled queue, waiting for an event.
    Paused = 1,
    /// Currently on the CPU.
    Running = 2,
    /// Dying, will be unscheduled and dropped at syscall boundary.
    TerminationPending = 3,
    /// Scheduled to be running.
    Scheduled = 4,
}

impl ThreadState {
    /// ThreadState is stored in the ThreadStruct as an AtomicUsize. This function casts it back to the enum.
    fn from_primitive(v: usize) -> ThreadState {
        match v {
            1 => ThreadState::Paused,
            2 => ThreadState::Running,
            3 => ThreadState::TerminationPending,
            4 => ThreadState::Scheduled,
            _ => panic!("Invalid thread state"),
        }
    }
}

impl ProcessStruct {
    /// Creates a new process.
    ///
    /// The created process will have no threads.
    ///
    /// # Panics
    ///
    /// Panics if max PID has been reached, which it shouldn't have since we're the first process.
    // todo: return an error instead of panicking
    pub fn new(procinfo: &ProcInfo, kacs: Option<&[u8]>) -> Result<Arc<ProcessStruct>, KernelError> {
        // allocate its memory space
        let pmemory = Mutex::new(ProcessMemory::default());

        // The PID.
        let pid = NEXT_PROCESS_ID.fetch_add(1, Ordering::SeqCst);
        if pid == usize::max_value() {
            panic!("Max PID reached!");
            // todo: return an error instead of panicking
        }

        let capabilities = if let Some(kacs) = kacs {
            ProcessCapabilities::parse_kcaps(kacs)?
        } else {
            ProcessCapabilities::default()
        };

        let p = Arc::new(
            ProcessStruct {
                pid,
                name: String::from_utf8_lossy(&procinfo.name).into_owned(),
                entrypoint: VirtualAddress(procinfo.code_addr as usize),
                pmemory,
                state: Mutex::new(ProcessStateData {
                    state: ProcessState::Created,
                    signaled: false,
                    waiting_threads: Vec::new(),
                    thread_maternity: Vec::new(),
                }),
                threads: SpinLockIRQ::new(Vec::new()),
                phandles: SpinLockIRQ::new(HandleTable::default()),
                tls_manager: Mutex::new(TLSManager::default()),
                capabilities
            }
        );

        PROCESS_LIST.lock().push(Arc::downgrade(&p));

        Ok(p)
    }

    /// Creates the initial thread, allocates the stack, and starts the process.
    ///
    /// # Errors
    ///
    /// - `ThreadAlreadyStarted`
    ///   - Process is already started.
    /// - `ProcessKilled` if the process struct was tagged `killed` before we
    ///    had time to start it.
    /// - `MemoryExhausted`
    ///    - Failed to allocate stack or thread TLS.
    pub fn start(this: &Arc<Self>, _main_thread_priority: u32, stack_size: usize) -> Result<(), UserspaceError> {

        // Lock state mutex.
        let mut statelock = this.state.lock();

        // ResourceLimit + 1 thread => 0x10801
        // Check imageSize + mainThreadStackSize + stackSize > memoryUsageCapacity => 0xD001 MemoryExhaustion

        let oldstate = statelock.state;
        if oldstate != ProcessState::Created && oldstate != ProcessState::CreatedAttached {
            return Err(UserspaceError::InvalidState);
        }

        // ResourceLimit reserve align_up(stackSize, PAGE_SIZE) memory
        // Allocate stack within new map region.
        let stack_size = sunrise_libutils::align_up(stack_size, PAGE_SIZE);
        let mut pmem = this.pmemory.lock();
        let stack_addr = pmem.find_available_space(stack_size)?;
        pmem.create_regular_mapping(stack_addr, stack_size, MemoryType::Stack, MappingAccessRights::u_rw())?;
        core::mem::drop(pmem);

        // Set self.mainThreadStackSize = stack_size.

        // self.heapCapacity = self.memory_capacity - self.image_size - self.mainThreadStackSize;
        // Initialize handle table - Done in the new function in SunriseOS.
        let first_thread = ThreadStruct::new_locked(this, &mut *statelock, this.entrypoint, stack_addr + stack_size, None)?;
        // InitForUser(), need to figure out what this does
        // This is actually done by ThreadStruct::new_locked for us:
        // this.phandles.lock().add_handle(Arc::new(Handle::Thread(first_thread.clone())));
        // SetEntryArguments - done in ThreadStruct::start for us.

        // Set to new state.
        statelock.set_state(match oldstate {
            ProcessState::Created => ProcessState::Started,
            ProcessState::CreatedAttached => ProcessState::StartedAttached,
            _ => unreachable!()
        });

        let first_thread = Weak::upgrade(&first_thread).unwrap();
        if let Err(err) = ThreadStruct::start_locked(&first_thread, &mut *statelock) {
            // Start failed, go back to Created state. We don't undo the
            // allocation of the stack. Nintendo doesn't either.
            //
            // Nintendo signals when re-entering created state. Weird.
            statelock.set_state(oldstate);

            return Err(err.into());
        }
        Ok(())
    }

    /// Gets the state of this process.
    pub fn state(&self) -> ProcessState {
        // Note: In nintendo, this code is *always* protected by a critical
        // section on the g_sched object. We don't have that, so instead we go
        // through a lock surrounding the ProcessStruct.
        self.state.lock().state
    }

    /// Clears the signaled state of this process.
    ///
    /// If the state is Exited, this function will return an error and the
    /// process will remain signaled.
    ///
    /// # Errors
    ///
    /// - `InvalidState`
    ///   - Process is not currently signaled.
    ///   - Process is in Exited state.
    pub fn clear_signal(&self) -> Result<(), KernelError> {
        let mut statelock = self.state.lock();
        if statelock.signaled && statelock.state != ProcessState::Exited {
            statelock.signaled = false;
            Ok(())
        } else {
            Err(KernelError::InvalidState {
                backtrace: Backtrace::new()
            })
        }
    }

    /// Creates the very first process at boot.
    /// Called internally by create_first_thread.
    ///
    /// The created process will have no threads.
    ///
    /// # Safety
    ///
    /// Use only for creating the very first process. Should never be used again after that.
    /// Must be using a valid KernelStack, a valid ActivePageTables.
    ///
    /// # Panics
    ///
    /// Panics if max PID has been reached, which it shouldn't have since we're the first process.
    unsafe fn create_first_process() -> ProcessStruct {

        // get the bootstrap hierarchy so we can free it
        let bootstrap_pages = InactiveHierarchy::from_currently_active();

        // create a new page table hierarchy for this process
        let mut pmemory = ProcessMemory::default();
        pmemory.switch_to();

        // free the bootstrap page tables
        drop(bootstrap_pages);

        let pid = NEXT_PROCESS_ID.fetch_add(1, Ordering::SeqCst);
        if pid == usize::max_value() {
            panic!("Max PID reached!");
        }

        ProcessStruct {
                pid,
                name: String::from("init"),
                entrypoint: VirtualAddress(0),
                pmemory: Mutex::new(pmemory),
                threads: SpinLockIRQ::new(Vec::new()),
                phandles: SpinLockIRQ::new(HandleTable::default()),
                state: Mutex::new(ProcessStateData {
                    signaled: false,
                    state: ProcessState::Started,
                    waiting_threads: Vec::new(),
                    thread_maternity: Vec::new(),
                }),
                tls_manager: Mutex::new(TLSManager::default()),
                capabilities: ProcessCapabilities::default(),
        }
    }

    /// Kills the current process by killing all of its threads.
    ///
    /// When a thread is about to return to userspace, it checks if its state is Killed.
    /// In this case it unschedules itself instead, and its ThreadStruct is dropped.
    ///
    /// When our last thread is dropped, our process struct is dropped with it.
    ///
    /// We also mark the process struct as killed to prevent race condition with
    /// another thread that would want to spawn a thread after we killed all ours.
    pub fn kill_current_process() {
        let this = scheduler::get_current_process();
        let statelock = this.state.lock();

        // Enter critical section.
        if ![ProcessState::Started, ProcessState::StartedAttached, ProcessState::DebugSuspended]
            .contains(&statelock.state)
        {
            // Leave critical section.
            // In nintendo's code, the flow here is a bit weird. It kills the
            // current thread. I find this a bit weird. If we call
            // kill_current_process while not started, then something is clearly
            // wrong in the kernel. So we'll panic instead.
            panic!("Invalid state! Attempted to kill the current process while it wasn't started: {:?}.", statelock.state);
        }

        // Normally, has the following flow:
        // statelock.state = ProcessState::Exiting;
        // Leave critical section.
        // drop(statelock);
        // KProcess::MaskThreads0x70Until(curThread)
        // Destroy handle table.
        // KMailBox::SendMail(0, KProcess::KMail)
        // KThread::Exit(curThread)

        // The SendMail will cause a kernel thread to wake up and
        // (eventually) finalize the process, moving it to the exited
        // state. Its flow is:
        // KProcess::MaskThreads0x70Until(0)
        // KProcess::SignalExitToDebugExited()
        // KProcess::SignalExit()

        // We'll simply kill our subthreads for now.
        this.kill_subthreads(statelock);
    }

    /// Kill all the subthreads of this process, and set the state to exited.
    fn kill_subthreads(&self, mut statelock: crate::sync::mutex::MutexGuard<ProcessStateData>) {
        // We're going to make things a **lot** simpler. We're just
        // going to immediately set ourselves as exiting, kill our
        // threads, and set ourselves as exited.
        statelock.set_state(ProcessState::Exiting);

        // kill our baby threads. Those threads have never run, we don't even bother
        // scheduling them so the can free their resources, just drop the hole maternity.
        statelock.thread_maternity.clear();
        drop(statelock);

        // kill all other regular threads
        for weak_thread in self.threads.lock().iter() {
            if let Some(t) = Weak::upgrade(weak_thread) {
                ThreadStruct::exit(t);
            }
        }

        self.state.lock().set_state(ProcessState::Exited);
    }

    /// Kills the given process, terminating the execution of all of its thread and
    /// putting its state to Exiting/Exited.
    ///
    /// Returns an error if used on a process that wasn't started.
    ///
    /// # Errors
    ///
    /// - `InvalidState`
    ///   - The process wasn't started (it is in Created or CreatedAttached state).
    pub fn terminate(&self) -> Result<(), KernelError> {
        let statelock = self.state.lock();
        match statelock.state {
            ProcessState::Created | ProcessState::CreatedAttached => {
                return Err(KernelError::InvalidState { backtrace: Backtrace::new() })
            },
            ProcessState::Started | ProcessState::StartedAttached |
            ProcessState::Crashed | ProcessState::DebugSuspended => {
                // We're supposed to do this:
                // KProcess::MaskThreads0x70Until(self, curthread)
                // Destroy handle table.
                // KProcess::SignalExitToDebugExited(self)
                // KProcess::SignalExit(self)

                // Let's do the simple thing:
                self.kill_subthreads(statelock);
            },
            ProcessState::Exiting | ProcessState::Exited => {
                // Already exiting, do nothing.
            }
            _ => {

            }
        }
        Ok(())
    }
}

impl Waitable for Arc<ProcessStruct> {
    fn is_signaled(&self) -> bool {
        self.state.lock().signaled
    }
    fn register(&self) {
        self.state.lock().waiting_threads.push(scheduler::get_current_thread());
    }
}

impl Drop for ProcessStruct {
    fn drop(&mut self) {
        // todo this should be a debug !
        PROCESS_LIST.lock().retain(|elem| elem.upgrade().is_some());
        info!("☠️ Dropped a process : {}", self.name)
    }
}

impl ThreadStruct {
    /// Creates a new thread.
    ///
    /// Sets the entrypoint and userspace stack pointer.
    ///
    /// Adds itself to list of threads of the belonging process.
    ///
    /// The returned thread will be in `Stopped` state.
    ///
    /// The thread's only strong reference is stored in the process' maternity,
    /// and we return only a weak to it, that can directly be put in a thread_handle.
    ///
    /// ##### Argument
    ///
    /// * When creating a new thread from `svcCreateThread` you should pass `Some(thread_entry_arg)`.
    ///   This should be the argument provided by the userspace, and will be passed to the thread
    ///   when it starts.
    /// * When creating the first thread of a process ("main thread") you should pass `None`.
    ///   This function will recognise this condition, automatically push a handle to the created
    ///   thread in the process' handle table, and this handle will be given as an argument to
    ///   the thread itself when it starts, so that the main thread can know its thread handle.
    pub fn new(belonging_process: &Arc<ProcessStruct>, ep: VirtualAddress, stack: VirtualAddress, arg: Option<usize>) -> Result<Weak<Self>, KernelError> {
        Self::new_locked(belonging_process, &mut *belonging_process.state.lock(), ep, stack, arg)
    }

    /// See [ThreadStruct::new]. Takes the ProcessStruct.data pre-locked to
    /// avoid deadlocks in [ProcessStruct::start()].
    fn new_locked(belonging_process: &Arc<ProcessStruct>, belonging_process_data: &mut ProcessStateData, ep: VirtualAddress, stack: VirtualAddress, arg: Option<usize>) -> Result<Weak<Self>, KernelError> {
        // get its process memory
        let mut pmemory = belonging_process.pmemory.lock();

        // allocate its kernel stack
        let kstack = KernelStack::allocate_stack()?;

        // hardware context will be computed later in this function, write a dummy value for now
        let empty_hwcontext = SpinLockIRQ::new(ThreadHardwareContext::default());

        // the state of the process, Paused
        let state = Atomic::new(ThreadState::Paused);

        // allocate its thread local storage region
        let tls = belonging_process.tls_manager.lock().allocate_tls(&mut pmemory)?;

        let t = Arc::new(
            ThreadStruct {
                state,
                kstack,
                hwcontext : empty_hwcontext,
                process: Arc::clone(belonging_process),
                tls_region: tls,
                tls_elf: SpinLock::new(VirtualAddress(0x00000000)),
                userspace_hwcontext: SpinLockIRQ::new(UserspaceHardwareContext::default()),
                state_event: ThreadStateEvent {
                    waiting_threads: SpinLock::new(Vec::new())
                },
            }
        );

        // if we're creating the main thread, push a handle to it in the process' handle table,
        // and give it to the thread as an argument.
        let args = match arg {
            Some(arg) => (arg, 0),
            None => {
                debug_assert!(belonging_process.threads.lock().is_empty() &&
                              belonging_process_data.thread_maternity.is_empty(), "Argument shouldn't be None");
                let handle = belonging_process.phandles.lock().add_handle(Arc::new(Handle::Thread(Arc::downgrade(&t))));

                (0, handle as usize)
            }
        };

        // prepare the thread's stack for its first schedule-in
        unsafe {
            // Safety: We just created the ThreadStruct, and own the only reference
            // to it, so we *know* it never has been scheduled, and cannot be.
            prepare_for_first_schedule(&t, ep.addr(), args, stack.addr());
        }

        // make a weak copy that we will return
        let ret = Arc::downgrade(&t);

        // add it to the process' list of threads, and to the maternity, simultaneously
        if belonging_process_data.state == ProcessState::Exited {
            // process was killed while we were waiting for the lock.
            // do not add the process to the vec, cancel the thread creation.
            drop(t);
            return Err(KernelError::ProcessKilled { backtrace: Backtrace::new() })
        }
        let mut threads_vec = belonging_process.threads.lock();
        // push a weak in the threads_vec
        threads_vec.push(Arc::downgrade(&t));
        // and put the only strong in the maternity
        belonging_process_data.thread_maternity.push(t);

        Ok(ret)
    }

    /// Creates the very first process and thread at boot.
    ///
    /// 1: Creates a process struct that uses current page tables.
    /// 2: Creates a thread struct that uses the current KernelStack as its stack,
    ///    and points to the created process.
    /// 3: Adds itself to the created process.
    ///
    /// Thread will be in state Running.
    ///
    /// Returns the created thread.
    ///
    /// # Safety
    ///
    /// Use only for creating the very first process. Should never be used again after that.
    /// Must be using a valid KernelStack, a valid ActivePageTables.
    ///
    /// Does not check that the process struct is not marked killed.
    ///
    /// # Panics
    ///
    /// Panics if max PID has been reached, which it shouldn't have since we're the first process.
    pub unsafe fn create_first_thread() -> Arc<ThreadStruct> {

        // first create the process we will belong to
        let mut process = ProcessStruct::create_first_process();

        // the state of the process, currently running
        let state = Atomic::new(ThreadState::Running);

        // use the already allocated stack
        let kstack = KernelStack::get_current_stack();

        // the saved esp will be overwritten on schedule-out anyway
        let hwcontext = SpinLockIRQ::new(ThreadHardwareContext::default());

        // create our thread local storage region
        let tls = {
            let pmemory = process.pmemory.get_mut();
            process.tls_manager.get_mut().allocate_tls(pmemory).expect("Failed to allocate TLS for first thread")
        };

        // we're done mutating the ProcessStruct, Arc it
        let process = Arc::new(process);

        let t = Arc::new(
            ThreadStruct {
                state,
                kstack,
                hwcontext,
                process: Arc::clone(&process),
                tls_region: tls,
                tls_elf: SpinLock::new(VirtualAddress(0x00000000)),
                userspace_hwcontext: SpinLockIRQ::new(UserspaceHardwareContext::default()),
                state_event: ThreadStateEvent {
                    waiting_threads: SpinLock::new(Vec::new())
                },
            }
        );

        // first process now has one thread
        process.threads.lock().push(Arc::downgrade(&t));

        t
    }

    /// See [ThreadStruct::start]. Takes the ProcessStruct.data pre-locked to
    /// avoid deadlocks in [ProcessStruct::start()].
    #[allow(clippy::needless_pass_by_value)] // more readable
    fn start_locked(thread: &Arc<Self>, belonging_process_data: &mut ProcessStateData) -> Result<(), KernelError> {
        // remove it from the maternity
        if belonging_process_data.state == ProcessState::Exited {
            // process was killed while we were waiting for the lock.
            // do not start process to the vec, cancel the thread start.
            return Err(KernelError::ProcessKilled { backtrace: Backtrace::new() })
        }
        let maternity = &mut belonging_process_data.thread_maternity;
        let cradle = maternity.iter().position(|baby| Arc::ptr_eq(baby, &thread));
        match cradle {
            None => {
                // the thread was not found in the maternity, meaning it had already started.
                Err(KernelError::InvalidState { backtrace: Backtrace::new() })
            },
            Some(pos) => {
                // remove it from maternity, and put it in the schedule queue
                scheduler::add_to_schedule_queue(maternity.remove(pos));
                Ok(())
            }
        }
    }

    /// Takes a reference to a thread, removes it from the maternity, and adds it to the schedule queue.
    ///
    /// We take only a weak reference, to permit calling this function easily with a thread_handle.
    ///
    /// # Errors
    ///
    /// * `ThreadAlreadyStarted` if the weak resolution failed (the thread has already been killed).
    /// * `ThreadAlreadyStarted` if the thread was not found in the maternity.
    /// * `ProcessKilled` if the process struct was tagged `killed` before we had time to start it.
    #[allow(clippy::needless_pass_by_value)] // more readable
    pub fn start(this: Weak<Self>) -> Result<(), KernelError> {
        let thread = this.upgrade().ok_or(
            // the thread was dropped, meaning it has already been killed.
            KernelError::InvalidState { backtrace: Backtrace::new() }
        )?;
        Self::start_locked(&thread, &mut *thread.process.state.lock())?;
        Ok(())
    }

    /// Sets the thread to the `Exited` state.
    ///
    /// We reschedule the thread (cancelling any waiting it was doing).
    /// In this state, the thread will die when attempting to return to userspace.
    ///
    /// If the thread was already in the `Exited` state, this function is a no-op.
    pub fn exit(this: Arc<Self>) {
        let old_state = this.state.swap(ThreadState::TerminationPending, Ordering::SeqCst);
        if old_state == ThreadState::TerminationPending {
            // if the thread was already marked exited, don't do anything.
            return;
        }

        // Signal that we are exited.
        this.state_event.signal();

        scheduler::add_to_schedule_queue(this);
    }
}

impl Drop for ThreadStruct {
    /// Late thread death notifications:
    ///
    /// * notifies our process that our TLS can be re-used.
    fn drop(&mut self) {
        unsafe {
            // safe: we're being dropped, our TLS will not be reused by us.
            self.process.tls_manager.lock().free_tls(self.tls_region);
        }
        // todo this should be a debug !
        info!("💀 Dropped a thread : {}", self.process.name)
    }
}
