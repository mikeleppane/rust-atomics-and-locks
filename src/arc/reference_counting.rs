mod basic {
    use std::ops::Deref;
    use std::ptr::NonNull;
    use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
    use std::sync::atomic::{fence, AtomicUsize};

    struct ArcData<T> {
        ref_count: AtomicUsize,
        data: T,
    }

    pub struct Arc<T> {
        ptr: NonNull<ArcData<T>>,
    }

    unsafe impl<T: Send + Sync> Send for Arc<T> {}

    unsafe impl<T: Send + Sync> Sync for Arc<T> {}

    impl<T> Arc<T> {
        pub fn new(data: T) -> Arc<T> {
            Arc {
                ptr: NonNull::from(Box::leak(Box::new(ArcData {
                    ref_count: AtomicUsize::new(1),
                    data,
                }))),
            }
        }

        fn data(&self) -> &ArcData<T> {
            unsafe { self.ptr.as_ref() }
        }

        pub fn get_mut(arc: &mut Self) -> Option<&mut T> {
            if arc.data().ref_count.load(Relaxed) == 1 {
                fence(Acquire);
                // Safety: Nothing else can access the data, since
                // there's only one Arc, to which we have exclusive access.
                unsafe { Some(&mut arc.ptr.as_mut().data) }
            } else {
                None
            }
        }
    }

    impl<T> Deref for Arc<T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.data().data
        }
    }

    impl<T> Clone for Arc<T> {
        fn clone(&self) -> Self {
            // TODO: Handle overflows.
            let current_rc = self.data().ref_count.fetch_add(1, Relaxed);
            if current_rc > usize::MAX / 2 {
                std::process::abort();
            }
            Self { ptr: self.ptr }
        }
    }

    impl<T> Drop for Arc<T> {
        fn drop(&mut self) {
            let current_rc = self.data().ref_count.fetch_sub(1, Release);
            if current_rc == 1 {
                fence(Acquire);
                unsafe {
                    drop(Box::from_raw(self.ptr.as_ptr()));
                }
            }
        }
    }

    #[test]
    fn test() {
        static NUM_DROPS: AtomicUsize = AtomicUsize::new(0);
        struct DetectDrop;
        impl Drop for DetectDrop {
            fn drop(&mut self) {
                NUM_DROPS.fetch_add(1, Relaxed);
            }
        }
        // Create two Arcs sharing an object containing a string
        // and a DetectDrop, to detect when it's dropped.
        let x = Arc::new(("hello", DetectDrop));
        let y = x.clone();
        // Send x to another thread, and use it there.
        let t = std::thread::spawn(move || {
            assert_eq!(x.0, "hello");
        });
        // In parallel, y should still be usable here.
        assert_eq!(y.0, "hello");

        // Wait for the thread to finish.
        t.join().unwrap();
        // One Arc, x, should be dropped by now.
        // We still have y, so the object shouldn't have been dropped yet.
        assert_eq!(NUM_DROPS.load(Relaxed), 0);
        // Drop the remaining `Arc`.
        drop(y);
        // Now that `y` is dropped too,
        // the object should've been dropped.
        assert_eq!(NUM_DROPS.load(Relaxed), 1);
    }
}

mod with_weak {
    use std::cell::UnsafeCell;
    use std::ops::Deref;
    use std::ptr::NonNull;
    use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
    use std::sync::atomic::{fence, AtomicUsize};

    struct ArcData<T> {
        /// Number of `Arc`s.
        data_ref_count: AtomicUsize,
        /// Number of `Arc`s and `Weak`s combined.
        alloc_ref_count: AtomicUsize,
        /// The data. `None` if there's only weak pointers left.
        data: UnsafeCell<Option<T>>,
    }

    pub struct Arc<T> {
        weak: Weak<T>,
    }

    pub struct Weak<T> {
        ptr: NonNull<ArcData<T>>,
    }

    unsafe impl<T: Sync + Send> Send for Weak<T> {}

    unsafe impl<T: Sync + Send> Sync for Weak<T> {}

    impl<T> Arc<T> {
        pub fn new(data: T) -> Arc<T> {
            Arc {
                weak: Weak {
                    ptr: NonNull::from(Box::leak(Box::new(ArcData {
                        alloc_ref_count: AtomicUsize::new(1),
                        data_ref_count: AtomicUsize::new(1),
                        data: UnsafeCell::new(Some(data)),
                    }))),
                },
            }
        }

        pub fn get_mut(arc: &mut Self) -> Option<&mut T> {
            if arc.weak.data().alloc_ref_count.load(Relaxed) == 1 {
                fence(Acquire);
                // Safety: Nothing else can access the data, since
                // there's only one Arc, to which we have exclusive access,
                // and no Weak pointers.
                let arcdata = unsafe { arc.weak.ptr.as_mut() };
                let option = arcdata.data.get_mut();
                // We know the data is still available since we
                // have an Arc to it, so this won't panic.
                let data = option.as_mut().unwrap();
                Some(data)
            } else {
                None
            }
        }

        pub fn downgrade(arc: &Self) -> Weak<T> {
            arc.weak.clone()
        }
    }

    impl<T> Weak<T> {
        fn data(&self) -> &ArcData<T> {
            unsafe { self.ptr.as_ref() }
        }

        pub fn upgrade(&self) -> Option<Arc<T>> {
            let mut n = self.data().data_ref_count.load(Relaxed);
            loop {
                if n == 0 {
                    return None;
                }
                assert!(n < usize::MAX);
                if let Err(e) =
                    self.data()
                        .data_ref_count
                        .compare_exchange_weak(n, n + 1, Relaxed, Relaxed)
                {
                    n = e;
                    continue;
                }
                return Some(Arc { weak: self.clone() });
            }
        }
    }

    impl<T> Deref for Arc<T> {
        type Target = T;

        fn deref(&self) -> &T {
            let ptr = self.weak.data().data.get();
            // Safety: Since there's an Arc to the data,
            // the data exists and may be shared.
            unsafe { (*ptr).as_ref().unwrap() }
        }
    }

    impl<T> Clone for Weak<T> {
        fn clone(&self) -> Self {
            if self.data().alloc_ref_count.fetch_add(1, Relaxed) > usize::MAX / 2 {
                std::process::abort();
            }
            Weak { ptr: self.ptr }
        }
    }

    impl<T> Clone for Arc<T> {
        fn clone(&self) -> Self {
            let weak = self.weak.clone();
            if weak.data().data_ref_count.fetch_add(1, Relaxed) > usize::MAX / 2 {
                std::process::abort();
            }
            Arc { weak }
        }
    }

    impl<T> Drop for Weak<T> {
        fn drop(&mut self) {
            if self.data().alloc_ref_count.fetch_sub(1, Release) == 1 {
                fence(Acquire);
                unsafe {
                    drop(Box::from_raw(self.ptr.as_ptr()));
                }
            }
        }
    }

    impl<T> Drop for Arc<T> {
        fn drop(&mut self) {
            if self.weak.data().data_ref_count.fetch_sub(1, Release) == 1 {
                fence(Acquire);
                let ptr = self.weak.data().data.get();
                // Safety: The data reference counter is zero,
                // so nothing will access it.
                unsafe {
                    (*ptr) = None;
                }
            }
        }
    }

    #[test]
    fn test() {
        static NUM_DROPS: AtomicUsize = AtomicUsize::new(0);
        struct DetectDrop;
        impl Drop for DetectDrop {
            fn drop(&mut self) {
                NUM_DROPS.fetch_add(1, Relaxed);
            }
        }
        // Create an Arc with two weak pointers.
        let x = Arc::new(("hello", DetectDrop));
        let y = Arc::downgrade(&x);
        let z = Arc::downgrade(&x);
        let t = std::thread::spawn(move || {
            // Weak pointer should be upgradable at this point.
            let y = y.upgrade().unwrap();
            assert_eq!(y.0, "hello");
        });
        assert_eq!(x.0, "hello");
        t.join().unwrap();
        // The data shouldn't be dropped yet,
        // and the weak pointer should be upgradable.
        assert_eq!(NUM_DROPS.load(Relaxed), 0);
        assert!(z.upgrade().is_some());
        drop(x);

        // Now, the data should be dropped, and the
        // weak pointer should no longer be upgradable.
        assert_eq!(NUM_DROPS.load(Relaxed), 1);
        assert!(z.upgrade().is_none());
    }
}

mod better_weak {
    use std::cell::UnsafeCell;
    use std::mem::ManuallyDrop;
    use std::ops::Deref;
    use std::ptr::NonNull;
    use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
    use std::sync::atomic::{fence, AtomicUsize};

    struct ArcData<T> {
        /// Number of `Arc`s.
        data_ref_count: AtomicUsize,
        /// Number of `Weak`s, plus one if there are any `Arc`s.
        alloc_ref_count: AtomicUsize,
        /// The data. Dropped if there are only weak pointers left.
        data: UnsafeCell<ManuallyDrop<T>>,
    }

    pub struct Arc<T> {
        ptr: NonNull<ArcData<T>>,
    }

    unsafe impl<T: Sync + Send> Send for Arc<T> {}

    unsafe impl<T: Sync + Send> Sync for Arc<T> {}

    pub struct Weak<T> {
        ptr: NonNull<ArcData<T>>,
    }

    unsafe impl<T: Sync + Send> Send for Weak<T> {}

    unsafe impl<T: Sync + Send> Sync for Weak<T> {}

    impl<T> Arc<T> {
        pub fn new(data: T) -> Arc<T> {
            Arc {
                ptr: NonNull::from(Box::leak(Box::new(ArcData {
                    alloc_ref_count: AtomicUsize::new(1),
                    data_ref_count: AtomicUsize::new(1),
                    data: UnsafeCell::new(ManuallyDrop::new(data)),
                }))),
            }
        }

        fn data(&self) -> &ArcData<T> {
            unsafe { self.ptr.as_ref() }
        }
    }

    impl<T> Deref for Arc<T> {
        type Target = T;

        fn deref(&self) -> &T {
            // Safety: Since there's an Arc to the data,
            // the data exists and may be shared.
            unsafe { &*self.data().data.get() }
        }
    }

    impl<T> Weak<T> {
        fn data(&self) -> &ArcData<T> {
            unsafe { self.ptr.as_ref() }
        }

        pub fn upgrade(&self) -> Option<Arc<T>> {
            let mut n = self.data().data_ref_count.load(Relaxed);
            loop {
                if n == 0 {
                    return None;
                }
                assert!(n < usize::MAX);
                if let Err(e) =
                    self.data()
                        .data_ref_count
                        .compare_exchange_weak(n, n + 1, Relaxed, Relaxed)
                {
                    n = e;
                    continue;
                }
                return Some(Arc { ptr: self.ptr });
            }
        }

        pub fn get_mut(arc: &mut Self) -> Option<&mut T> {
            // Acquire matches Weak::drop's Release decrement, to make sure any
            // upgraded pointers are visible in the next data_ref_count.load.
            if arc
                .data()
                .alloc_ref_count
                .compare_exchange(1, usize::MAX, Acquire, Relaxed)
                .is_err()
            {
                return None;
            }
            let is_unique = arc.data().data_ref_count.load(Relaxed) == 1;
            // Release matches Acquire increment in `downgrade`, to make sure any
            // changes to the data_ref_count that come after `downgrade` don't
            // change the is_unique result above.
            arc.data().alloc_ref_count.store(1, Release);
            if !is_unique {
                return None;
            }
            // Acquire to match Arc::drop's Release decrement, to make sure nothing
            // else is accessing the data.
            fence(Acquire);
            unsafe { Some(&mut *arc.data().data.get()) }
        }
    }

    impl<T> Clone for Weak<T> {
        fn clone(&self) -> Self {
            if self.data().alloc_ref_count.fetch_add(1, Relaxed) > usize::MAX / 2 {
                std::process::abort();
            }
            Weak { ptr: self.ptr }
        }
    }

    impl<T> Drop for Weak<T> {
        fn drop(&mut self) {
            if self.data().alloc_ref_count.fetch_sub(1, Release) == 1 {
                fence(Acquire);
                unsafe {
                    drop(Box::from_raw(self.ptr.as_ptr()));
                }
            }
        }
    }

    impl<T> Clone for Arc<T> {
        fn clone(&self) -> Self {
            if self.data().data_ref_count.fetch_add(1, Relaxed) > usize::MAX / 2 {
                std::process::abort();
            }
            Arc { ptr: self.ptr }
        }
    }

    impl<T> Drop for Arc<T> {
        fn drop(&mut self) {
            if self.data().data_ref_count.fetch_sub(1, Release) == 1 {
                fence(Acquire);
                // Safety: The data reference counter is zero,
                // so nothing will access the data anymore.
                unsafe {
                    ManuallyDrop::drop(&mut *self.data().data.get());
                }
                // Now that there's no `Arc<T>`s left,
                // drop the implicit weak pointer that represented all `Arc<T>`s.
                drop(Weak { ptr: self.ptr });
            }
        }
    }
}
