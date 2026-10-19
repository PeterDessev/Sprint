//! The event bus: subscribing is cheap and memory grows only with the
//! events actually queued.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sprint_host::event::EVENT_QUEUE_LIMIT;
use sprint_host::{Event, EventBus};

/// Counts the bytes allocated by this test binary.
struct Counting;

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn subscribing_allocates_little_and_queues_stay_bounded() {
    let bus = EventBus::default();
    let before = ALLOCATED.load(Ordering::Relaxed);
    let receivers: Vec<_> = (0..8).map(|_| bus.subscribe()).collect();
    let used = ALLOCATED.load(Ordering::Relaxed) - before;
    // A bounded channel of EVENT_QUEUE_LIMIT events would take ~13 MB each.
    assert!(used < 64 * 1024, "8 subscribers allocated {used} bytes");

    // A subscriber that never reads keeps at most the limit; one that
    // reads gets everything.
    let reader = bus.subscribe();
    let mut read = 0usize;
    for i in 0..EVENT_QUEUE_LIMIT + 100 {
        bus.emit(Event::SettingsUpdated);
        if i % 1000 == 0 {
            read += reader.try_iter().count();
        }
    }
    read += reader.try_iter().count();
    assert_eq!(read, EVENT_QUEUE_LIMIT + 100);
    for rx in &receivers {
        assert_eq!(rx.len(), EVENT_QUEUE_LIMIT);
    }
    // Dropping a stuck receiver frees its queue; it is forgotten.
    drop(receivers);
    bus.emit(Event::SettingsUpdated);
    assert_eq!(reader.try_iter().count(), 1);
}
