//! Check allocation traffic separately from retained memory or frame timings.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use awob_core::{bindings::Bindings, render::Renderer, theme};

struct CountingAllocator;
thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every operation forwards its pointer and layout unchanged to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.get() {
            BYTES.set(BYTES.get() + layout.size());
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if ENABLED.get() {
            BYTES.set(BYTES.get() + layout.size());
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.get() {
            BYTES.set(BYTES.get() + size);
        }
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocated_bytes(render: impl FnOnce()) -> usize {
    BYTES.set(0);
    ENABLED.set(true);
    render();
    ENABLED.set(false);
    BYTES.get()
}

// Count only this thread: the test harness can allocate concurrently even in
// a binary with a single test.
#[test]
fn warm_cached_frame_avoids_surface_allocation() {
    let theme = theme::parse("surface { width 360; height 64; }; scene {}").unwrap();
    let bindings = Bindings::default();
    let mut renderer = Renderer::new();
    renderer.render_cached(&theme, &bindings, None).unwrap();
    let owned = allocated_bytes(|| {
        std::hint::black_box(renderer.render(&theme, &bindings, None).unwrap());
    });
    let cached = allocated_bytes(|| {
        std::hint::black_box(renderer.render_cached(&theme, &bindings, None).unwrap());
    });
    assert_eq!(owned, 360 * 64 * 4);
    assert_eq!(cached, 0);
}
