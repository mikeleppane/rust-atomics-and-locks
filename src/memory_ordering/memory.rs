#![allow(dead_code)]

mod rel_acq {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering::{Acquire, Release};
    use std::thread;

    static mut DATA: String = String::new();
    static LOCKED: AtomicBool = AtomicBool::new(false);

    fn f() {
        if !LOCKED.swap(true, Acquire) {
            // Safety: We hold the exclusive lock, so nothing else is accessing DATA.
            unsafe { DATA.push('!') };
            LOCKED.store(false, Release);
        }
    }

    fn main() {
        thread::scope(|s| {
            for _ in 0..100 {
                s.spawn(f);
            }
        });
        unsafe { println!("{DATA}") }
    }

    #[test]
    fn test_values_from_thin_air() {
        main()
    }
}

mod lazy_init {
    use std::sync::atomic::AtomicPtr;
    use std::sync::atomic::Ordering::{Acquire, Release};
    use std::thread;
    use std::thread::current;

    use rand::RngCore;

    struct Data {
        id: u64,
    }

    fn generate_data() -> Data {
        let mut rng = rand::thread_rng();
        Data { id: rng.next_u64() }
    }

    fn get_data() -> &'static Data {
        static PTR: AtomicPtr<Data> = AtomicPtr::new(std::ptr::null_mut());
        let mut p = PTR.load(Acquire);
        if p.is_null() {
            p = Box::into_raw(Box::new(generate_data()));
            if let Err(e) = PTR.compare_exchange(std::ptr::null_mut(), p, Release, Acquire) {
                // Safety: p comes from Box::into_raw right above,
                // and wasn't shared with any other thread.
                drop(unsafe { Box::from_raw(p) });
                p = e;
            }
        }
        // Safety: p is not null and points to a properly initialized value.
        unsafe { &*p }
    }

    #[test]
    fn test_get_data() {
        thread::scope(|s| {
            s.spawn(|| {
                let data = get_data();
                let thread_id = current().id();
                println!("From thread {:?}: {}", thread_id, data.id)
            });
            s.spawn(|| {
                let data = get_data();
                let thread_id = current().id();
                println!("From thread {:?}: {}", thread_id, data.id)
            });
            let data = get_data();
            let thread_id = current().id();
            println!("From main thread {:?}: {}", thread_id, data.id)
        });
    }
}

mod seq_cst {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering::SeqCst;
    use std::thread;

    static A: AtomicBool = AtomicBool::new(false);
    static B: AtomicBool = AtomicBool::new(false);
    static mut S: String = String::new();

    fn main() {
        let a = thread::spawn(|| {
            A.store(true, SeqCst);
            if !B.load(SeqCst) {
                unsafe { S.push('!') };
            }
        });
        let b = thread::spawn(|| {
            B.store(true, SeqCst);
            if !A.load(SeqCst) {
                unsafe { S.push('!') };
            }
        });
        a.join().unwrap();
        b.join().unwrap();
        unsafe { println!("{S}") }
    }

    #[test]
    fn test_main() {
        main()
    }
}

mod fences {
    use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
    use std::sync::atomic::{fence, AtomicBool};
    use std::thread;
    use std::time::Duration;

    static mut DATA: [u64; 10] = [0; 10];
    const ATOMIC_FALSE: AtomicBool = AtomicBool::new(false);
    static READY: [AtomicBool; 10] = [ATOMIC_FALSE; 10];

    fn some_calculation(index: usize) -> u64 {
        index as u64
    }

    fn main() {
        for i in 0..10 {
            thread::spawn(move || {
                let data = some_calculation(i);
                unsafe { DATA[i] = data };
                READY[i].store(true, Release);
            });
        }
        thread::sleep(Duration::from_millis(500));
        let ready: [bool; 10] = std::array::from_fn(|i| READY[i].load(Relaxed));
        if ready.contains(&true) {
            fence(Acquire);
            for i in 0..10 {
                if ready[i] {
                    println!("data{i} = {}", unsafe { DATA[i] });
                }
            }
        }
    }

    #[test]
    fn test_main() {
        main()
    }
}
