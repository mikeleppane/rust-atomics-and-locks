mod simple_channel {
    use std::collections::VecDeque;
    use std::sync::{Condvar, Mutex};

    pub struct Channel<T> {
        queue: Mutex<VecDeque<T>>,
        item_ready: Condvar,
    }

    impl<T> Channel<T> {
        pub fn new() -> Self {
            Self {
                queue: Mutex::new(VecDeque::new()),
                item_ready: Condvar::new(),
            }
        }
        pub fn send(&self, message: T) {
            self.queue.lock().unwrap().push_back(message);
            self.item_ready.notify_one();
        }
        pub fn receive(&self) -> T {
            let mut b = self.queue.lock().unwrap();
            loop {
                if let Some(message) = b.pop_front() {
                    return message;
                }
                b = self.item_ready.wait(b).unwrap();
            }
        }
    }
}

mod one_shot_channel {
    use std::cell::UnsafeCell;
    use std::mem::MaybeUninit;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
    use std::thread;

    pub struct Channel<T> {
        message: UnsafeCell<MaybeUninit<T>>,
        ready: AtomicBool,
        in_use: AtomicBool,
    }

    unsafe impl<T> Sync for Channel<T> where T: Send {}

    impl<T> Channel<T> {
        pub const fn new() -> Self {
            Self {
                message: UnsafeCell::new(MaybeUninit::uninit()),
                ready: AtomicBool::new(false),
                in_use: AtomicBool::new(false),
            }
        }

        /// Panics when trying to send more than one message.
        pub fn send(&self, message: T) {
            if self.in_use.swap(true, Relaxed) {
                panic!("can't send more than one message!");
            }
            unsafe {
                (*self.message.get()).write(message);
            }
            self.ready.store(true, Release);
        }

        pub fn is_ready(&self) -> bool {
            self.ready.load(Relaxed)
        }

        /// Panics if no message is available yet.
        ///
        /// Tip: Use `is_ready` to check first.
        ///
        /// Safety: Only call this once!
        pub fn receive(&self) -> T {
            if !self.ready.swap(false, Acquire) {
                panic!("no message available!");
            }

            // Safety: We've just checked (and reset) the ready flag.
            unsafe { (*self.message.get()).assume_init_read() }
        }
    }

    impl<T> Drop for Channel<T> {
        fn drop(&mut self) {
            if *self.ready.get_mut() {
                unsafe { self.message.get_mut().assume_init_drop() }
            }
        }
    }

    #[test]
    fn test_one_shot_channel_with_parking() {
        let channel = Channel::new();
        let t = thread::current();
        thread::scope(|s| {
            s.spawn(|| {
                channel.send("hello world!");
                t.unpark();
            });
            while !channel.is_ready() {
                thread::park();
            }
            assert_eq!(channel.receive(), "hello world!");
        });
    }

    #[test]
    #[should_panic]
    fn test_one_shot_channel_calling_send_twice_should_panic() {
        let channel = Channel::new();
        let t = thread::current();
        thread::scope(|s| {
            s.spawn(|| {
                channel.send("hello world!");
                channel.send("");
                t.unpark();
            });
            while !channel.is_ready() {
                thread::park();
            }
            assert_eq!(channel.receive(), "hello world!");
        });
    }
}

mod sender_receiver_channel_with_arc {
    use std::cell::UnsafeCell;
    use std::mem::MaybeUninit;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
    use std::sync::Arc;
    use std::thread;

    pub struct Sender<T> {
        channel: Arc<Channel<T>>,
    }

    pub struct Receiver<T> {
        channel: Arc<Channel<T>>,
    }

    struct Channel<T> {
        // no longer `pub`
        message: UnsafeCell<MaybeUninit<T>>,
        ready: AtomicBool,
    }

    unsafe impl<T> Sync for Channel<T> where T: Send {}

    pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
        let a = Arc::new(Channel {
            message: UnsafeCell::new(MaybeUninit::uninit()),
            ready: AtomicBool::new(false),
        });
        (Sender { channel: a.clone() }, Receiver { channel: a })
    }

    impl<T> Sender<T> {
        /// This never panics. :)
        pub fn send(self, message: T) {
            unsafe { (*self.channel.message.get()).write(message) };
            self.channel.ready.store(true, Release);
        }
    }

    impl<T> Receiver<T> {
        pub fn is_ready(&self) -> bool {
            self.channel.ready.load(Relaxed)
        }
        pub fn receive(self) -> T {
            if !self.channel.ready.swap(false, Acquire) {
                panic!("no message available!");
            }
            unsafe { (*self.channel.message.get()).assume_init_read() }
        }
    }

    impl<T> Drop for Channel<T> {
        fn drop(&mut self) {
            if *self.ready.get_mut() {
                unsafe { self.message.get_mut().assume_init_drop() }
            }
        }
    }

    #[test]
    #[should_panic]
    fn test_sender_receiver() {
        thread::scope(|s| {
            let (sender, receiver) = channel();
            let t = thread::current();
            s.spawn(move || {
                sender.send("hello world!");
                // sender.send(""); => this will not compile
                t.unpark();
            });
            while !receiver.is_ready() {
                thread::park();
            }
            assert_eq!(receiver.receive(), "hello world!");
        });
    }
}

mod sender_receiver_channel_with_borrowing {
    use std::cell::UnsafeCell;
    use std::mem::MaybeUninit;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
    use std::thread;

    pub struct Channel<T> {
        message: UnsafeCell<MaybeUninit<T>>,
        ready: AtomicBool,
    }

    unsafe impl<T> Sync for Channel<T> where T: Send {}

    pub struct Sender<'a, T> {
        channel: &'a Channel<T>,
    }

    pub struct Receiver<'a, T> {
        channel: &'a Channel<T>,
    }

    impl<T> Channel<T> {
        pub const fn new() -> Self {
            Self {
                message: UnsafeCell::new(MaybeUninit::uninit()),
                ready: AtomicBool::new(false),
            }
        }
        pub fn split(&mut self) -> (Sender<T>, Receiver<T>) {
            *self = Self::new();
            (Sender { channel: self }, Receiver { channel: self })
        }
    }

    impl<T> Sender<'_, T> {
        pub fn send(self, message: T) {
            unsafe { (*self.channel.message.get()).write(message) };
            self.channel.ready.store(true, Release);
        }
    }

    impl<T> Receiver<'_, T> {
        pub fn is_ready(&self) -> bool {
            self.channel.ready.load(Relaxed)
        }

        pub fn receive(self) -> T {
            if !self.channel.ready.swap(false, Acquire) {
                panic!("no message available!");
            }
            unsafe { (*self.channel.message.get()).assume_init_read() }
        }
    }

    impl<T> Drop for Channel<T> {
        fn drop(&mut self) {
            if *self.ready.get_mut() {
                unsafe { self.message.get_mut().assume_init_drop() }
            }
        }
    }

    #[test]
    fn test_sender_receiver() {
        let mut channel = Channel::new();
        thread::scope(|s| {
            let (sender, receiver) = channel.split();
            let t = thread::current();
            s.spawn(move || {
                sender.send("hello world!");
                t.unpark();
            });
            while !receiver.is_ready() {
                thread::park();
            }
            assert_eq!(receiver.receive(), "hello world!");
        });
    }
}
