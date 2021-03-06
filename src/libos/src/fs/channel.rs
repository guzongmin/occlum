use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Weak;

use ringbuf::{Consumer as RbConsumer, Producer as RbProducer, RingBuffer};

use super::{IoEvents, IoNotifier};
use crate::events::{Event, EventFilter, Notifier, Observer, Waiter, WaiterQueueObserver};
use crate::prelude::*;

/// A unidirectional communication channel, intended to implement IPC, e.g., pipe,
/// unix domain sockets, etc.
pub struct Channel<I> {
    producer: Producer<I>,
    consumer: Consumer<I>,
}

impl<I> Channel<I> {
    /// Create a new channel.
    pub fn new(capacity: usize) -> Result<Self> {
        let state = Arc::new(State::new());

        let rb = RingBuffer::new(capacity);
        let (rb_producer, rb_consumer) = rb.split();
        let producer = Producer::new(rb_producer, state.clone());
        let consumer = Consumer::new(rb_consumer, state.clone());

        // Make event connection between the producer and consumer
        producer.notifier().register(
            Arc::downgrade(&consumer.observer) as Weak<dyn Observer<_>>,
            None,
            None,
        );
        consumer.notifier().register(
            Arc::downgrade(&producer.observer) as Weak<dyn Observer<_>>,
            None,
            None,
        );

        Ok(Self { producer, consumer })
    }

    /// Push an item into the channel.
    pub fn push(&self, item: I) -> Result<()> {
        self.producer.push(item)
    }

    /// Push an non-copy item into the channel.
    ///
    /// Non-copy items need special treatment because once passed as an argument
    /// to this method, an non-copy object is considered **moved** from the
    /// caller to the callee (this method) by Rust. This makes it impossible for
    /// the caller to retry calling this method with the same input item
    /// in case of an `EAGAIN` or `EINTR` error. For this reason, we need a way
    /// for the caller to get back the ownership of the input item upon error.
    /// Thus, an extra argument is added to this method.
    // TODO: implement this method in the future when pushing items individually is
    // really needed
    pub fn push_noncopy(&self, item: I, retry: &mut Option<I>) -> Result<()> {
        unimplemented!();
    }

    /// Pop an item out of the channel.
    pub fn pop(&self) -> Result<Option<I>> {
        self.consumer.pop()
    }

    /// Turn the channel into a pair of producer and consumer.
    pub fn split(self) -> (Producer<I>, Consumer<I>) {
        let Channel { producer, consumer } = self;
        (producer, consumer)
    }
}

impl<I: Copy> Channel<I> {
    /// Push a slice of items into the channel.
    pub fn push_slice(&self, items: &[I]) -> Result<usize> {
        self.producer.push_slice(items)
    }

    /// Pop a slice of items from the channel.
    pub fn pop_slice(&self, items: &mut [I]) -> Result<usize> {
        self.consumer.pop_slice(items)
    }
}

/// An endpoint is either the producer or consumer of a channel.
pub struct EndPoint<T> {
    inner: SgxMutex<T>,
    state: Arc<State>,
    observer: Arc<WaiterQueueObserver<IoEvents>>,
    notifier: IoNotifier,
    is_nonblocking: AtomicBool,
}

impl<T> EndPoint<T> {
    fn new(inner: T, state: Arc<State>) -> Self {
        let inner = SgxMutex::new(inner);
        let observer = WaiterQueueObserver::new();
        let notifier = IoNotifier::new();
        let is_nonblocking = AtomicBool::new(false);
        Self {
            inner,
            state,
            observer,
            notifier,
            is_nonblocking,
        }
    }

    /// Returns the I/O notifier.
    ///
    /// An interesting observer can receive I/O events of the endpoint by
    /// registering itself to this notifier.
    pub fn notifier(&self) -> &IoNotifier {
        &self.notifier
    }

    /// Returns whether the endpoint is non-blocking.
    ///
    /// By default, a channel is blocking.
    pub fn is_nonblocking(&self) -> bool {
        self.is_nonblocking.load(Ordering::Acquire)
    }

    /// Set whether the endpoint is non-blocking.
    pub fn set_nonblocking(&self, nonblocking: bool) {
        self.is_nonblocking.store(nonblocking, Ordering::Release);

        if nonblocking {
            // Wake all threads that are blocked on pushing/popping this endpoint
            self.observer.waiter_queue().dequeue_and_wake_all();
        }
    }
}

/// The state of a channel shared by the two endpoints of a channel.
struct State {
    is_producer_shutdown: AtomicBool,
    is_consumer_shutdown: AtomicBool,
}

impl State {
    pub fn new() -> Self {
        Self {
            is_producer_shutdown: AtomicBool::new(false),
            is_consumer_shutdown: AtomicBool::new(false),
        }
    }

    pub fn is_producer_shutdown(&self) -> bool {
        self.is_producer_shutdown.load(Ordering::Acquire)
    }

    pub fn is_consumer_shutdown(&self) -> bool {
        self.is_consumer_shutdown.load(Ordering::Acquire)
    }

    pub fn set_producer_shutdown(&self) {
        self.is_producer_shutdown.store(true, Ordering::Release)
    }

    pub fn set_consumer_shutdown(&self) {
        self.is_consumer_shutdown.store(true, Ordering::Release)
    }
}

// Just like a normal loop, except that a waiter queue (as well as a waiter)
// is used to avoid busy loop. This macro is used in the push/pop implementation
// below.
macro_rules! waiter_loop {
    ($loop_body: block, $waiter_queue: expr) => {
        // Try without creating a waiter. This saves some CPU cycles if the
        // first attempt succeeds.
        {
            $loop_body
        }

        // The main loop
        let waiter = Waiter::new();
        let waiter_queue = $waiter_queue;
        loop {
            waiter_queue.reset_and_enqueue(&waiter);

            {
                $loop_body
            }

            waiter.wait(None)?;
        }
    };
}

/// Producer is the writable endpoint of a channel.
pub type Producer<I> = EndPoint<RbProducer<I>>;

impl<I> Producer<I> {
    pub fn push(&self, mut item: I) -> Result<()> {
        waiter_loop!(
            {
                let mut rb_producer = self.inner.lock().unwrap();
                if self.is_self_shutdown() || self.is_peer_shutdown() {
                    return_errno!(EPIPE, "one or both endpoints have been shutdown");
                }

                item = match rb_producer.push(item) {
                    Ok(()) => {
                        drop(rb_producer);
                        self.notifier.broadcast(&IoEvents::IN);
                        return Ok(());
                    }
                    Err(item) => item,
                };

                if self.is_nonblocking() {
                    return_errno!(EAGAIN, "try again later");
                }
            },
            self.observer.waiter_queue()
        );
    }

    pub fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();

        let writable = {
            let mut rb_producer = self.inner.lock().unwrap();
            !rb_producer.is_full()
        };
        if writable {
            events |= IoEvents::OUT;
        }

        if self.is_self_shutdown() {
            events |= IoEvents::HUP;
        }
        if self.is_peer_shutdown() {
            events |= IoEvents::RDHUP;
        }

        events
    }

    pub fn shutdown(&self) {
        {
            // It is important to hold this lock while updating the state
            let inner = self.inner.lock().unwrap();
            self.state.set_producer_shutdown();
        }

        // Notify all consumers and other observers
        self.notifier.broadcast(&IoEvents::HUP);
        // Wake all threads that are blocked on pushing to this producer
        self.observer.waiter_queue().dequeue_and_wake_all();
    }

    pub fn is_self_shutdown(&self) -> bool {
        self.state.is_producer_shutdown()
    }

    pub fn is_peer_shutdown(&self) -> bool {
        self.state.is_consumer_shutdown()
    }
}

impl<I: Copy> Producer<I> {
    pub fn push_slice(&self, items: &[I]) -> Result<usize> {
        waiter_loop!(
            {
                let mut rb_producer = self.inner.lock().unwrap();
                if self.is_self_shutdown() || self.is_peer_shutdown() {
                    return_errno!(EPIPE, "one or both endpoints have been shutdown");
                }

                let count = rb_producer.push_slice(items);
                if count > 0 {
                    drop(rb_producer);
                    self.notifier.broadcast(&IoEvents::IN);
                    return Ok(count);
                }

                if self.is_nonblocking() {
                    return_errno!(EAGAIN, "try again later");
                }
            },
            self.observer.waiter_queue()
        );
    }
}

/// Consumer is the readable endpoint of a channel.
pub type Consumer<I> = EndPoint<RbConsumer<I>>;

impl<I> Consumer<I> {
    pub fn pop(&self) -> Result<Option<I>> {
        waiter_loop!(
            {
                let mut rb_consumer = self.inner.lock().unwrap();
                if self.is_self_shutdown() {
                    return_errno!(EPIPE, "this endpoint has been shutdown");
                }

                if let Some(item) = rb_consumer.pop() {
                    drop(rb_consumer);
                    self.notifier.broadcast(&IoEvents::OUT);
                    return Ok(Some(item));
                }

                if self.is_peer_shutdown() {
                    return Ok(None);
                }
                if self.is_nonblocking() {
                    return_errno!(EAGAIN, "try again later");
                }
            },
            self.observer.waiter_queue()
        );
    }

    pub fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();

        let readable = {
            let mut rb_consumer = self.inner.lock().unwrap();
            !rb_consumer.is_empty()
        };
        if readable {
            events |= IoEvents::IN;
        }

        if self.is_self_shutdown() {
            events |= IoEvents::RDHUP;
        }
        if self.is_peer_shutdown() {
            events |= IoEvents::HUP;
        }

        events
    }

    pub fn shutdown(&self) {
        {
            // It is important to hold this lock while updating the state
            let inner = self.inner.lock().unwrap();
            self.state.set_consumer_shutdown();
        }

        // Notify all producers and other observers
        self.notifier.broadcast(&IoEvents::RDHUP);
        // Wake all threads that are blocked on popping from this consumer
        self.observer.waiter_queue().dequeue_and_wake_all();
    }

    pub fn is_self_shutdown(&self) -> bool {
        self.state.is_consumer_shutdown()
    }

    pub fn is_peer_shutdown(&self) -> bool {
        self.state.is_producer_shutdown()
    }
}

impl<I: Copy> Consumer<I> {
    pub fn pop_slice(&self, items: &mut [I]) -> Result<usize> {
        waiter_loop!(
            {
                let mut rb_consumer = self.inner.lock().unwrap();
                if self.is_self_shutdown() {
                    return_errno!(EPIPE, "this endpoint has been shutdown");
                }

                let count = rb_consumer.pop_slice(items);
                if count > 0 {
                    drop(rb_consumer);
                    self.notifier.broadcast(&IoEvents::OUT);
                    return Ok(count);
                };

                if self.is_peer_shutdown() {
                    return Ok(0);
                }
                if self.is_nonblocking() {
                    return_errno!(EAGAIN, "try again later");
                }
            },
            self.observer.waiter_queue()
        );
    }
}
