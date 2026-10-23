//! The app's allocator: the system's, with the heap counted. An allocation the system refuses (the
//! machine out of commit memory) is written to logs/memory.log before Rust aborts the process, which
//! otherwise ends it without a word; and a sampler logs each new high-water mark of the heap.
//!
//! ALM_MEM_TRACE=<MB> (diagnosis): each time a thread's own heap grows by another <MB>, its stack is
//! logged, which names what holds the memory.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::OnceLock;

pub struct Counting;

static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// logs/memory.log, opened before anything can fail: writing to it does not allocate
static FAIL_LOG: OnceLock<std::fs::File> = OnceLock::new();
/// ALM_MEM_TRACE in bytes (0: off)
static TRACE_STEP: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// this thread's allocations less its frees, and when its stack is logged next (trace only)
    static MINE: Cell<(isize, isize)> = const { Cell::new((0, 0)) };
    static TRACING: Cell<bool> = const { Cell::new(false) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if p.is_null() { failed(l.size()) } else { grew(l.size()) }
        p
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(l);
        if p.is_null() { failed(l.size()) } else { grew(l.size()) }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        shrank(l.size());
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if q.is_null() {
            failed(new);
        } else if new >= l.size() {
            grew(new - l.size());
        } else {
            shrank(l.size() - new);
        }
        q
    }
}

fn grew(n: usize) {
    let c = CUR.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(c, Relaxed);
    let step = TRACE_STEP.load(Relaxed);
    if step > 0 {
        traced(n as isize, step as isize);
    }
}

fn shrank(n: usize) {
    CUR.fetch_sub(n, Relaxed);
    if TRACE_STEP.load(Relaxed) > 0 {
        let _ = MINE.try_with(|m| {
            let (a, next) = m.get();
            m.set((a - n as isize, next));
        });
    }
}

fn traced(n: isize, step: isize) {
    let Ok((mine, next)) = MINE.try_with(|m| {
        let (a, next) = m.get();
        m.set((a + n, next));
        (a + n, next)
    }) else { return };
    if mine < next.max(step) || TRACING.try_with(|t| t.replace(true)).unwrap_or(true) {
        return;
    }
    let _ = MINE.try_with(|m| m.set((mine, (mine / step + 1) * step)));
    // allocates: TRACING keeps this from tracing itself. Straight to the file, not the logger: the
    // allocation may come from a thread that holds the logger's lock.
    if let Some(mut f) = FAIL_LOG.get() {
        let name = std::thread::current().name().unwrap_or("?").to_string();
        let _ = writeln!(f, "{} TRACE   alm.memory: thread {name} holds {} MB (heap {} MB) at:\n{}", crate::log::timestamp(), mine >> 20,
                         MB(CUR.load(Relaxed)), std::backtrace::Backtrace::force_capture());
    }
    let _ = TRACING.try_with(|t| t.set(false));
}

#[allow(non_snake_case)]
fn MB(b: usize) -> usize {
    b >> 20
}

/// The heap now and its high-water mark, in bytes.
pub fn usage() -> (usize, usize) {
    (CUR.load(Relaxed), PEAK.load(Relaxed))
}

/// A refused allocation, written without allocating: "<UTC time> allocation of N bytes failed ...".
fn failed(size: usize) {
    let Some(mut f) = FAIL_LOG.get() else { return };
    let mut buf = [0u8; 200];
    let mut w = Buf { b: &mut buf, n: 0 };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, mo, d, h, mi, s) = crate::log::civil(now);
    for (v, width, sep) in [(y as usize, 4, b"-"), (mo as usize, 2, b"-"), (d as usize, 2, b" "), (h as usize, 2, b":"), (mi as usize, 2, b":"), (s as usize, 2, b" ")] {
        w.num(v, width);
        w.put(sep);
    }
    w.put(b"ERROR   alm.memory: allocation of ");
    w.num(size, 1);
    w.put(b" bytes failed (the system is out of memory); heap ");
    w.num(MB(CUR.load(Relaxed)), 1);
    w.put(b" MB, peak ");
    w.num(MB(PEAK.load(Relaxed)), 1);
    w.put(b" MB; the app stops\n");
    let n = w.n;
    let _ = f.write_all(&buf[..n]);
    let _ = f.flush();
}

struct Buf<'a> {
    b: &'a mut [u8],
    n: usize,
}

impl Buf<'_> {
    fn put(&mut self, s: &[u8]) {
        let k = s.len().min(self.b.len() - self.n);
        self.b[self.n..self.n + k].copy_from_slice(&s[..k]);
        self.n += k;
    }

    fn num(&mut self, mut v: usize, width: usize) {
        let mut d = [b'0'; 20];
        let mut i = d.len();
        while v > 0 || d.len() - i < width.max(1) {
            i -= 1;
            d[i] = b'0' + (v % 10) as u8;
            v /= 10;
        }
        self.put(&d[i..]);
    }
}

/// At startup, once HOME is known: open logs/memory.log for a refused allocation, read
/// ALM_MEM_TRACE, and log the heap's high-water mark each time it rises by another 64 MB.
pub fn start() {
    let dir = crate::paths::logs();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("memory.log")) {
        let _ = FAIL_LOG.set(f);
    }
    if let Some(mb) = std::env::var("ALM_MEM_TRACE").ok().and_then(|v| v.parse::<usize>().ok()).filter(|m| *m > 0) {
        TRACE_STEP.store(mb << 20, Relaxed);
    }
    std::thread::Builder::new().name("memory".into()).spawn(|| {
        let mut logged = 0;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let (cur, peak) = usage();
            if peak >= logged + (64 << 20) {
                logged = peak;
                crate::info!("memory", "heap peak {} MB (now {} MB)", MB(peak), MB(cur));
            }
        }
    }).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_without_allocating_and_padded() {
        let mut buf = [0u8; 32];
        let mut w = Buf { b: &mut buf, n: 0 };
        w.num(7, 2);
        w.put(b" ");
        w.num(0, 1);
        w.put(b" ");
        w.num(123456, 1);
        let n = w.n;
        assert_eq!(&buf[..n], b"07 0 123456");
        // a full buffer cuts off instead of panicking
        let mut small = [0u8; 4];
        let mut w = Buf { b: &mut small, n: 0 };
        w.put(b"abcdef");
        assert_eq!(w.n, 4);
    }

    #[test]
    fn a_refused_allocation_is_written_as_a_log_line() {
        let p = std::env::temp_dir().join(format!("alm-test-memory-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&p);
        FAIL_LOG.set(std::fs::OpenOptions::new().create(true).append(true).open(&p).unwrap()).unwrap();
        failed(3 << 30);
        let s = std::fs::read_to_string(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        assert!(s.ends_with(" MB; the app stops\n"), "{s}");
        assert!(s.contains(" ERROR   alm.memory: allocation of 3221225472 bytes failed (the system is out of memory); heap "), "{s}");
        // "YYYY-MM-DD HH:MM:SS " first, as the other logs
        let b = s.as_bytes();
        assert!(b[..19].iter().enumerate().all(|(i, c)| match i { 4 | 7 => *c == b'-', 10 => *c == b' ', 13 | 16 => *c == b':', _ => c.is_ascii_digit() }), "{s}");
    }
}
