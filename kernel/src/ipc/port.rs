use scheduler;
use alloc::vec::Vec;
use alloc::sync::{Arc, Weak};
use sync::SpinLock;
use error::UserspaceError;
use event::{self, Waitable};
use process::ThreadStruct;
use core::sync::atomic::{AtomicUsize, Ordering};
use ipc::session::{self, ClientSession, ServerSession};

/// An endpoint which can be connected to.
#[derive(Debug)]
struct Port {
    incoming_connections: SpinLock<Vec<Arc<IncomingConnection>>>,
    accepters: SpinLock<Vec<Weak<ThreadStruct>>>,
    servercount: AtomicUsize,
}

/// The client side of a Port.
///
/// This side can call connect().
#[derive(Debug, Clone)]
pub struct ClientPort(Arc<Port>);

/// The server side of a Port.
///
/// This is necessary for accepting connections on a port. Its only operation
/// is accept(), which returns a ServerSession. It implements Waitable, which
/// waits until its associated ClientPort called connect().
#[derive(Debug)]
pub struct ServerPort(Arc<Port>);

impl Port {
    /// Creates a new port. This port may only have _max_sessions sessions active at
    /// a given time.
    fn new(_max_sessions: u32) -> (ServerPort, ClientPort) {
        let port = Arc::new(Port {
            servercount: AtomicUsize::new(0),
            incoming_connections: SpinLock::new(Vec::new()),
            accepters: SpinLock::new(Vec::new())
        });
        (Port::server(port.clone()), Port::client(port.clone()))
    }

    /// Returns a ClientPort from this Port.
    fn client(this: Arc<Self>) -> ClientPort {
        ClientPort(this)
    }

    /// Returns a ServerPort from this Port.
    fn server(this: Arc<Self>) -> ServerPort {
        this.servercount.fetch_add(1, Ordering::SeqCst);
        ServerPort(this)
    }
}

pub fn new(_max_sessions: u32) -> (ServerPort, ClientPort) {
    Port::new(_max_sessions)
}

// Wait for a connection to become available.
impl Waitable for ServerPort {
    fn is_signaled(&self) -> bool {
        !self.0.incoming_connections.lock().is_empty()
    }

    fn register(&self) {
        self.0.accepters.lock().push(Arc::downgrade(&scheduler::get_current_thread()));
    }
}

impl Clone for ServerPort {
    fn clone(&self) -> Self {
        assert!(self.0.servercount.fetch_add(1, Ordering::SeqCst) != usize::max_value(), "Overflow when incrementing servercount");
        ServerPort(self.0.clone())
    }
}

impl Drop for ServerPort {
    fn drop(&mut self) {
        let count = self.0.servercount.fetch_sub(1, Ordering::SeqCst);
        assert!(count != 0, "Overflow when decrementing servercount");
        if count == 1 {
            info!("Last ServerPort dropped");
            // We're dead jim.
            let mut internal = self.0.incoming_connections.lock();

            for request in internal.drain(..) {
                scheduler::add_to_schedule_queue(request.creator.clone());
            }
        }
    }
}

#[derive(Debug)]
pub struct IncomingConnection {
    session: SpinLock<Option<ClientSession>>,
    creator: Arc<ThreadStruct>
}

impl ServerPort {
    /// Accept a new connection on the Port.
    pub fn accept(&self) -> Result<ServerSession, UserspaceError> {
        loop {
            // Wait for incoming_connections to contain a connection.
            let _ = event::wait(Some(self as &dyn Waitable))?;

            // Acquire the connection.
            while let Some(incoming) = self.0.incoming_connections.lock().pop() {
                let mut lock = incoming.session.lock();

                // Check if it was already handled by another accepter!
                // This shouldn't happen since we pop it from the queue above.
                assert!(lock.is_none(), "Handled connection request still in incoming conn queue.");

                // We can associate a session to this now.
                let (server, client) = session::new();
                *lock = Some(client);

                // Wake up the creator.
                // **VERY IMPORTANT**: This should be done with the LOCK HELD!!!
                info!("Resuming {}", incoming.creator.process.name);
                scheduler::add_to_schedule_queue(incoming.creator.clone());

                // We're done!
                return Ok(server);
            }
        }
    }
}

impl ClientPort {
    /// Connects to this port.
    pub fn connect(&self) -> Result<ClientSession, UserspaceError> {
        let incoming = Arc::new(IncomingConnection {
            session: SpinLock::new(None),
            creator: scheduler::get_current_thread()
        });

        let mut guard = incoming.session.lock();
        let lock = self.0.incoming_connections.lock().push(incoming.clone());

        let session = loop {
            // If no handle to the server exist anymore, give up.
            if self.0.servercount.load(Ordering::SeqCst) == 0 {
                return Err(UserspaceError::PortRemoteDead);
            }

            // First, wake up an accepter
            while let Some(item) = self.0.accepters.lock().pop() {
                if let Some(thread) = item.upgrade() {
                    scheduler::add_to_schedule_queue(thread);
                    break;
                }
            }

            // Wait for it to do its job, and wake us up
            guard = scheduler::unschedule(&incoming.session, guard)?;

            // Make sure it did its job. If it didn't, try again.
            if let Some(s) = guard.take() {
                break s;
            }
        };

        Ok(session)
    }
}
