//! Core kernel types.
//!
//! This module contains all the core types of the libuser. Most objects in this
//! modules are newtypes around Handle.

use core::marker::PhantomData;
use crate::syscalls;
use core::num::NonZeroU32;
use sunrise_libkern::MemoryPermissions;
use sunrise_libkern::process::{ProcessState, ProcessInfoType};
use crate::error::{Error, KernelError};
use crate::ipc::{Message, MessageTy};
use crate::futures::WorkQueue;
use core::mem;

/// A Handle is a sort of reference to a Kernel Object. Its underlying
/// representation is that of a u32. Furthermore, an Option<Handle> is also
/// guaranteed to be represented on a u32, with None represented as 0. This
/// allows handle to be used directly in the syscall functions.
///
/// Handles are closed automatically when Dropped via [close_handle].
///
/// [close_handle]: crate::syscalls::close_handle.
#[repr(transparent)]
#[derive(Debug, PartialEq, Eq)]
pub struct Handle(pub(crate) NonZeroU32);

impl Handle {
    /// Creates a new handle from the given number. This number should come from
    /// a raw syscall. Constructing a handle from an arbitrary number is not
    /// unsafe, but may lead to extremely confusing code.
    pub fn new(handle: u32) -> Handle {
        Handle(NonZeroU32::new(handle).expect("Syscall returned handle 0!?!"))
    }

    /// Creates a new reference to this handle. See the documentation of
    /// [HandleRef] for more information.
    pub fn as_ref(&self) -> HandleRef<'_> {
        HandleRef {
            inner: self.0,
            lifetime: PhantomData
        }
    }

    /// Creates a new static reference to this handle. See the documentation of
    /// [HandleRef] for more information.
    ///
    /// The kernel guarantees that a Handle is never reused. If the parent [Handle]
    /// dies before this HandleRef is dropped, every function taking this HandleRef
    /// will fail with [sunrise_libkern::error::KernelError::InvalidHandle]
    pub fn as_ref_static(&self) -> HandleRef<'static> {
        HandleRef {
            inner: self.0,
            lifetime: PhantomData
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        match self.0.get() {
            0xFFFF8000 | 0xFFFF8001 => (),
            handle => { let _ = syscalls::close_handle(handle); },
        }
    }
}

/// A fake reference to a Handle. Has the same representation as a real Handle,
/// but is bound to the real handle's lifetime.
///
/// This pattern allows for passing handle arrays without giving up ownership of
/// the handle, and without an expensive conversion from an array of pointers to
/// an array of handles.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandleRef<'a> {
    /// The underlying handle number.
    pub(crate) inner: NonZeroU32,
    /// The real handle this reference is tied to.
    lifetime: PhantomData<&'a Handle>
}


impl<'a> HandleRef<'a> {
    /// Remove the lifetime on the current HandleRef. See [Handle::as_ref_static()] for
    /// more information on the safety of this operation.
    pub fn staticify(self) -> HandleRef<'static> {
        HandleRef {
            inner: self.inner,
            lifetime: PhantomData
        }
    }

    /// Returns a future that waits for the current handle to get signaled. This effectively
    /// registers the currently executing Task to be polled again by the future executor backing
    /// the given [WorkQueue] when this handle gets signaled.
    ///
    /// # Panics
    ///
    /// Panics if used from outside the context of a Future spawned on a libuser
    /// future executor. Please make sure you only call this function from a
    /// future spawned on a WaitableManager.
    pub fn wait_async(self, queue: WorkQueue<'_>)-> impl core::future::Future<Output = Result<(), Error>> + Unpin {
        #[allow(missing_docs, clippy::missing_docs_in_private_items)]
        struct MyFuture {
            queue: crate::futures::SimpleWorkQueue,
            handle: HandleRef<'static>,
            registered_on: Option<core::task::Waker>
        }
        impl core::future::Future for MyFuture {
            type Output = Result<(), Error>;
            fn poll(mut self: core::pin::Pin<&mut Self>, cx: &mut core::task::Context) -> core::task::Poll<Result<(), Error>> {
                match syscalls::wait_synchronization(&[self.handle], Some(0)) {
                    Err(KernelError::Timeout) => {
                        self.registered_on = Some(cx.waker().clone());
                        self.queue.wait_for(self.handle, cx);
                        core::task::Poll::Pending
                    },
                    Err(err) => core::task::Poll::Ready(Err(err.into())),
                    Ok(_) => core::task::Poll::Ready(Ok(()))
                }
            }
        }
        impl Drop for MyFuture {
            fn drop(&mut self) {
                if let Some(waker) = &self.registered_on {
                    self.queue.unwait_for(self.handle, waker.clone());
                }
            }
        }

        MyFuture {
            queue: queue.simple(), handle: self.staticify(), registered_on: None
        }
    }
}

/// A handle on an IRQ event.
#[repr(transparent)]
#[derive(Debug)]
pub struct IRQEvent(pub Handle);

/// The readable part of an event. The user shall use this end to verify if the
/// event is signaled, and wait for the signaling through wait_synchronization.
/// The user can also use this handle to clear the signaled state through
/// [ReadableEvent::clear()].
#[repr(transparent)]
#[derive(Debug)]
pub struct ReadableEvent(pub Handle);

impl ReadableEvent {
    /// Clears the signaled state.
    pub fn clear(&self) -> Result<(), KernelError> {
        syscalls::clear_event(self.0.as_ref())
    }

    /// Waits for the event to get signaled.
    ///
    /// Note: This function is a bit of a footgun. If you intend to have
    /// multiple futures wait on the same event (to use it like a semaphore),
    /// please look at [ReadableEvent::wait_async_cb()] instead.
    ///
    /// # Panics
    ///
    /// Panics if used from outside the context of a Future spawned on a libuser
    /// future executor. Please make sure you only call this function from a
    /// future spawned on a WaitableManager.
    pub fn wait_async(&self, queue: crate::futures::WorkQueue<'_>) -> impl core::future::Future<Output = Result<(), Error>> + Unpin {
        self.0.as_ref().wait_async(queue)
    }

    /// Turns this ReadableEvent into a semaphore-like structure.
    ///
    /// This function will repeatedly run `f` when the event is triggered, until
    /// it returns true. When it returns false, the future will first clear the
    /// event before waiting on it.
    ///
    /// # Panics
    ///
    /// Panics if used from outside the context of a Future spawned on a libuser
    /// future executor. Please make sure you only call this function from a
    /// future spawned on a WaitableManager.
    pub fn wait_async_cb<F, T>(&self, queue: crate::futures::WorkQueue<'_>, f: F) -> impl core::future::Future<Output = T> + Unpin
    where
        F: FnMut() -> Option<T> + Unpin,
    {
        #[allow(missing_docs, clippy::missing_docs_in_private_items)]
        struct MyFuture<F> {
            queue: crate::futures::SimpleWorkQueue,
            handle: HandleRef<'static>,
            registered_on: Option<core::task::Waker>,
            f: F
        }

        impl<F, T> core::future::Future for MyFuture<F>
        where
            F: FnMut() -> Option<T> + Unpin
        {
            type Output = T;
            fn poll(mut self: core::pin::Pin<&mut Self>, cx: &mut core::task::Context) -> core::task::Poll<T> {
                // TODO: Remove wait_async_cb workaround rust-lang/rust#65489
                // BODY: Rust seems to have a bit of a weird bug around the
                // BODY: interaction of DerefMut, Pin and FnMut. See
                // BODY: https://github.com/rust-lang/rust/issues/65489.
                let this = &mut *self;

                if let Some(s) = (this.f)() {
                    core::task::Poll::Ready(s)
                } else {
                    let _ = syscalls::clear_event(this.handle);
                    this.registered_on = Some(cx.waker().clone());
                    this.queue.wait_for(this.handle, cx);
                    core::task::Poll::Pending
                }
            }
        }
        impl<F> Drop for MyFuture<F> {
            fn drop(&mut self) {
                if let Some(waker) = &self.registered_on {
                    self.queue.unwait_for(self.handle, waker.clone());
                }
            }
        }

        MyFuture {
            queue: queue.simple(), handle: self.0.as_ref_static(),
            registered_on: None, f
        }
    }
}


/// The writable part of an event. The user shall use this end to signal (and
/// wake up threads waiting on the event).
#[derive(Debug)]
pub struct WritableEvent(pub Handle);

impl WritableEvent {
    /// Clears the signaled state.
    pub fn clear(&self) -> Result<(), KernelError> {
        syscalls::clear_event(self.0.as_ref())
    }

    /// Signals the event, setting its state to signaled and waking up any
    /// thread waiting on its value.
    pub fn signal(&self) -> Result<(), KernelError> {
        syscalls::signal_event(self)
    }
}

/// The client side of an IPC session.
///
/// Usually obtained by connecting to a service through the sm: service manager.
/// However, an anonymous session pair might be created through the
/// [create_session] syscall, or by calling [connect_to_named_port].
///
/// [create_session]: crate::syscalls::create_session
/// [connect_to_named_port]: crate::syscalls::connect_to_named_port
#[repr(transparent)]
#[derive(Debug)]
pub struct ClientSession(pub Handle);

impl ClientSession {
    /// Send an IPC request to the handle, and wait for a response. The passed
    /// buffer should contain the request on input, and will contain the reply
    /// on output.
    ///
    /// This is a low-level primitives that is usually wrapped by a higher-level
    /// library. Look at the [ipc module] for more information on the IPC
    /// message format.
    ///
    /// [ipc module]: crate::ipc
    pub fn send_sync_request_with_user_buffer(&self, buf: &mut [u8]) -> Result<(), Error> {
        syscalls::send_sync_request_with_user_buffer(buf, self)
            .map_err(|v| v.into())
    }

    /// Consumes the session, returning the underlying handle. Note that closing
    /// a Handle without sending a close IPC message will leak the object in the
    /// sysmodule. You should always reconstruct the ClientSession from the
    /// Handle before dropping it.
    pub fn into_handle(self) -> Handle {
        let handle = Handle((self.0).0);
        mem::forget(self);
        handle
    }

    /// Clones the current object, returning a new handle. The returned handle
    /// has its own IPC buffer - it may be used concurrently with the original.
    pub fn try_clone(&self) -> Result<ClientSession, Error> {
        let mut buf = [0; 0x100];
        let mut msg = Message::<(), [_; 0], [_; 0], [_; 0]>::new_request(None, 2);
        msg.set_ty(MessageTy::Control);
        msg.pack(&mut buf[..]);
        self.send_sync_request_with_user_buffer(&mut buf[..])?;
        let mut res: Message<'_, (), [_; 0], [_; 0], [_; 1]> = Message::unpack(&buf[..]);
        res.error()?;
        let handle = res.pop_handle_move()?;
        Ok(ClientSession(handle))
    }
}

impl Drop for ClientSession {
    fn drop(&mut self) {
        let mut buf = [0; 0x100];
		    let mut msg = Message::<(), [_; 0], [_; 0], [_; 0]>::new_request(None, 1);
        msg.set_ty(MessageTy::Close);
        msg.pack(&mut buf[..]);
		    let _ = self.send_sync_request_with_user_buffer(&mut buf[..]);
    }
}

/// The server side of an IPC session.
///
/// Usually obtained by calling [accept], but may also be obtained by calling
/// the [create_session] syscall, providing a server/client session pair.
///
/// [accept]: ServerPort::accept
/// [create_session]: crate::syscalls::create_session
#[repr(transparent)]
#[derive(Debug)]
pub struct ServerSession(pub Handle);

impl ServerSession {
    /// Receives an IPC request from the session, waiting if none are available
    /// yet. The buffer should contain an empty message, optionally containing a
    /// C descriptor, and will contain the reply on output.
    ///
    /// If a C descriptor is provided, it will be used as the buffer to copy the
    /// request's X descriptor into.
    ///
    /// This is a low-level primitives that is usually wrapped by a higher-level
    /// library. Look at the [ipc module] for more information on the IPC
    /// message format.
    ///
    /// [ipc module]: crate::ipc
    pub fn receive(&self, buf: &mut [u8], timeout: Option<usize>) -> Result<(), Error> {
        syscalls::reply_and_receive_with_user_buffer(buf, &[self.0.as_ref()], None, timeout).map(|_| ())
            .map_err(|v| v.into())
    }

    /// Replies to an IPC request on the given session. If the given session did
    /// not have a pending request, this function will error out.
    ///
    /// This is a low-level primitives that is usually wrapped by a higher-level
    /// library. Look at the [ipc module] for more information on the IPC
    /// message format.
    ///
    /// [ipc module]: crate::ipc
    pub fn reply(&self, buf: &mut [u8]) -> Result<(), Error> {
        syscalls::reply_and_receive_with_user_buffer(buf, &[], Some(self.0.as_ref()), Some(0))
            .map(|_| ())
            .or_else(|v| if KernelError::Timeout == v {
                Ok(())
            } else {
                Err(v)
            })
            .map_err(|v| v.into())
    }

    /// Waits for the server to receive a request.
    ///
    /// Once this function returns, calling [ServerSession::receive()] is
    /// guaranteed not to block.
    ///
    /// # Panics
    ///
    /// Panics if used from outside the context of a Future spawned on a libuser
    /// future executor. Please make sure you only call this function from a
    /// future spawned on a WaitableManager.
    pub fn wait_async(&self, queue: crate::futures::WorkQueue<'_>) -> impl core::future::Future<Output = Result<(), Error>> + Unpin {
        self.0.as_ref().wait_async(queue)
    }
}

/// The client side of an IPC Port. Allows connecting to an IPC server, providing
/// a session to call remote procedures on.
///
/// Obtained by creating an anonymous port pair with the [create_port] syscall.
///
/// [create_port]: crate::syscalls::create_port
#[repr(transparent)]
#[derive(Debug)]
pub struct ClientPort(pub Handle);

impl ClientPort {
    /// Connects to a port, returning a session on which to send IPC request.
    pub fn connect(&self) -> Result<ClientSession, Error> {
        syscalls::connect_to_port(self)
            .map_err(|v| v.into())
    }
}

/// The server side of an IPC Port. Allows listening for connections, providing
/// a session on which to answer remote procedures from.
///
/// Usually obtained by registering a service through the sm: service manager, or
/// by calling [manage_named_port] to obtained a kernel-managed port.
///
/// [manage_named_port]: crate::syscalls::manage_named_port
#[repr(transparent)]
#[derive(Debug)]
pub struct ServerPort(pub Handle);

impl ServerPort {
    /// Accepts a connection to the port, returning a server session on which to
    /// listen and reply to IPC request.
    pub fn accept(&self) -> Result<ServerSession, Error> {
        syscalls::accept_session(self)
            .map_err(|v| v.into())
    }

    /// Waits for the server to receive a connection.
    ///
    /// Once this function returns, the next call to [ServerPort::accept()] is
    /// guaranteed not to block. Attention: Because accept does not have any
    /// non-blocking mode, it is dangerous to share a ServerPort across multiple
    /// futures or threads (since multiple threads or futures will get woken up
    /// and attempt accepting, but only one accept will not block).
    ///
    /// If you wish to wait on a server port from multiple threads, please
    /// ensure that calls to the accept functions are wrapped in a mutex.
    ///
    /// # Panics
    ///
    /// Panics if used from outside the context of a Future spawned on a libuser
    /// future executor. Please make sure you only call this function from a
    /// future spawned on a WaitableManager.
    // TODO: Footgun: Sharing ServerPorts can result in blocking the event loop
    // BODY: If the user shares ServerPorts across threads/futures and does
    // BODY: something like calling accept straight after wait_async, they might
    // BODY: end up blocking the event loop. This is because two threads might
    // BODY: race for the call to accept after the wait_async.
    pub fn wait_async(&self, queue: crate::futures::WorkQueue<'_>) -> impl core::future::Future<Output = Result<(), Error>> + Unpin {
        self.0.as_ref().wait_async(queue)
    }
}

/// A Thread. Created with the [create_thread syscall].
///
/// See the [threads] module.
///
/// [create_thread syscall]: crate::syscalls::create_thread.
/// [threads]: crate::threads
#[repr(transparent)]
#[derive(Debug)]
pub struct Thread(pub Handle);

impl Thread {
    /// Gets the current process handle. Uses the 0xFFFF8000 meta-handle, which
    /// may not be valid in all contexts!
    fn current() -> Thread {
        Thread(Handle::new(0xFFFF8000))
    }
}

/// A Process. Created with `create_process` syscall, or by calling
/// [Process::current()].
#[repr(transparent)]
#[derive(Debug)]
pub struct Process(pub Handle);

impl Process {
    /// Gets the current process handle. Uses the 0xFFFF8001 meta-handle, which
    /// may not be valid in all contexts!
    fn current() -> Process {
        Process(Handle::new(0xFFFF8001))
    }

    /// Start the given process on the provided CPU with the provided scheduler
    /// priority.
    ///
    /// A stack of the given size will be allocated using the process' memory
    /// resource limit and memory pool.
    ///
    /// The entrypoint is assumed to be the first address of the `code_addr` region
    /// provided in [`create_process`](crate::syscalls::create_process()). It
    /// takes two parameters: the first is the usermode exception handling
    /// context, and should always be NULL. The second is a handle to the main
    /// thread.
    ///
    /// # Errors
    ///
    /// - `InvalidProcessorId`
    ///   - Attempted to start the process on a processor that doesn't exist on the
    ///     current machine, or a processor that the process is not allowed to use.
    /// - `InvalidThreadPriority`
    ///   - Attempted to use a priority above 0x3F, or a priority that the created
    ///     process is not allowed to use.
    /// - `MemoryFull`
    ///   - Provided stack size is bigger than available vmem space.
    pub fn start(&self, main_thread_prio: u32, default_cpuid: u32, main_thread_stack_sz: u32) -> Result<(), Error> {
        syscalls::start_process(self, main_thread_prio, default_cpuid, main_thread_stack_sz)
            .map_err(|v| v.into())
    }

    /// Get the state the given process is currently in.
    ///
    /// Shouldn't ever return an error, unless the user is doing weird things
    /// with handles.
    pub fn state(&self) -> Result<ProcessState, Error> {
        let info = syscalls::get_process_info(self, ProcessInfoType::ProcessState)?;
        Ok(ProcessState(info as u8))
    }

    /// Waits for the process to change state. Use [Process::state] to get the
    /// new state and [Process::reset_signal] to reset the signaled state.
    ///
    /// # Panics
    ///
    /// Panics if used from outside the context of a Future spawned on a libuser
    /// future executor. Please make sure you only call this function from a
    /// future spawned on a WaitableManager.
    pub fn wait_async(&self, queue: crate::futures::WorkQueue<'_>) -> impl core::future::Future<Output = Result<(), Error>> + Unpin {
        self.0.as_ref().wait_async(queue)
    }

    /// Clear the "signaled" state of a process. A process moves to the signaled
    /// state when it changes `ProcessState` (e.g. when exiting).
    ///
    /// Note that once a Process enters the Exited state, it is permanently
    /// signaled and cannot be reset. Calling `reset_signal` will return an
    /// InvalidState error.
    ///
    /// # Errors
    ///
    /// - `InvalidState`
    ///   - The event wasn't signaled.
    ///   - The process was in Exited state.
    pub fn reset_signal(&self) -> Result<(), Error> {
        syscalls::reset_signal(self.0.as_ref())?;
        Ok(())
    }

    /// Gets the [Pid] of this Process.
    ///
    /// Will return an `InvalidHandle` error if called on `Process::current()`.
    ///
    /// # Errors
    ///
    /// - `InvalidHandle`
    ///   - Called in `Process::current()`.
    pub fn pid(&self) -> Result<Pid, Error> {
        let pid = syscalls::get_process_id(self)?;
        Ok(Pid(pid))
    }
}

/// A handle to memory that may be mapped in multiple processes at the same time.
///
/// Special care should be used to ensure multiple processes do not write to the
/// memory at the same time, or only does so through the use of atomic
/// operations. Otherwise, UB will occur!
#[repr(transparent)]
#[derive(Debug)]
pub struct SharedMemory(pub Handle);

impl SharedMemory {
    /// Creates a new Shared Memory handle. The physical memory underlying this
    /// shared memory will span `length` bytes.
    ///
    /// Myperm and otherperm are masks of which permissions are allowed when
    /// mapping the shared memory in the current process and other processes
    /// respectively.
    pub fn new(length: usize, myperm: MemoryPermissions, otherperm: MemoryPermissions) -> Result<SharedMemory, Error> {
        syscalls::create_shared_memory(length, myperm, otherperm)
            .map_err(|v| v.into())
    }

    /// Maps the current shared memory at the given address, consuming the handle
    /// and returning a MappedMemoryRegion. Note that the size must be equal to
    /// the length of the SharedMemory.
    pub fn map(self, addr: usize, size: usize, perm: MemoryPermissions) -> Result<MappedSharedMemory, Error> {
        syscalls::map_shared_memory(&self, addr, size, perm)?;
        Ok(MappedSharedMemory {
            handle: self,
            addr,
            size,
            perm
        })
    }
}

/// A mapping to a shared memory region.
///
/// When dropped, the memory region will be unmapped, and the SharedMemory handle
/// associated with it will be closed.
#[derive(Debug)]
#[allow(clippy::missing_docs_in_private_items)]
pub struct MappedSharedMemory {
    handle: SharedMemory,
    addr: usize,
    size: usize,
    perm: MemoryPermissions
}

#[allow(clippy::len_without_is_empty)] // len cannot be zero.
impl MappedSharedMemory {
    /// Gets a raw pointer to the underlying shared memory.
    ///
    /// The pointer is valid until the MappedSharedMemory instance gets dropped.
    pub fn as_ptr(&self) -> *const u8 {
        self.addr as *const u8
    }

    /// Gets a mutable raw pointer to the underlying shared memory.
    ///
    /// The pointer is valid until the MappedSharedMemory instance gets dropped.
    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.addr as *mut u8
    }

    /// Gets the byte length of the mapped shared memory.
    pub fn len(&self) -> usize {
        self.size
    }

    /// Return a reference to the underlying shared memory. Useful to send a copy
    /// of the handle of an already mapped shared memory via IPC.
    pub fn as_shared_mem(&self) -> &SharedMemory {
        &self.handle
    }
}

impl Drop for MappedSharedMemory {
    fn drop(&mut self) {
        unsafe {
            // Safety: If this is dropped, then all references given out to the
            // data pointed to by addr should have been dropped as well.
            let _ = syscalls::unmap_shared_memory(&self.handle, self.addr, self.size);
        }
    }
}

/// Process ID, as returned by IPC.
///
/// Each process in Horizon is given a unique, non-reusable PID. It may be used
/// to associate capabilities or resources to a particular process. For instance,
/// sm might associate a process' service access permissions to its pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(pub u64);
