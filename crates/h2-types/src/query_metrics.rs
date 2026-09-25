use std::cell::RefCell;
use std::time::Duration;

/// One statement's storage activity. Durations are wall-clock nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryCounters {
    pub lock_wait_ns: u64,
    pub tree_lock_wait_ns: u64,
    pub wal_lock_wait_ns: u64,
    pub commit_lock_wait_ns: u64,
    pub wal_write_ns: u64,
    pub wal_sync_ns: u64,
    pub wal_durable_wait_ns: u64,
    pub point_gets: u64,
    pub scans: u64,
    pub scan_entries: u64,
}

impl QueryCounters {
    pub fn since(self, earlier: Self) -> Self {
        Self {
            lock_wait_ns: self.lock_wait_ns.saturating_sub(earlier.lock_wait_ns),
            tree_lock_wait_ns: self.tree_lock_wait_ns.saturating_sub(earlier.tree_lock_wait_ns),
            wal_lock_wait_ns: self.wal_lock_wait_ns.saturating_sub(earlier.wal_lock_wait_ns),
            commit_lock_wait_ns: self.commit_lock_wait_ns.saturating_sub(earlier.commit_lock_wait_ns),
            wal_write_ns: self.wal_write_ns.saturating_sub(earlier.wal_write_ns),
            wal_sync_ns: self.wal_sync_ns.saturating_sub(earlier.wal_sync_ns),
            wal_durable_wait_ns: self.wal_durable_wait_ns.saturating_sub(earlier.wal_durable_wait_ns),
            point_gets: self.point_gets.saturating_sub(earlier.point_gets),
            scans: self.scans.saturating_sub(earlier.scans),
            scan_entries: self.scan_entries.saturating_sub(earlier.scan_entries),
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<QueryCounters>> = const { RefCell::new(None) };
}

pub struct QueryMetricsGuard {
    previous: Option<QueryCounters>,
    active: bool,
}

impl QueryMetricsGuard {
    pub fn start() -> Self {
        let previous = CURRENT.with(|cell| cell.replace(Some(QueryCounters::default())));
        Self { previous, active: true }
    }

    pub fn snapshot() -> QueryCounters {
        CURRENT.with(|cell| cell.borrow().unwrap_or_default())
    }

    pub fn finish(mut self) -> QueryCounters {
        let counters = Self::snapshot();
        CURRENT.with(|cell| {
            cell.replace(self.previous.take());
        });
        self.active = false;
        counters
    }
}

impl Drop for QueryMetricsGuard {
    fn drop(&mut self) {
        if self.active {
            CURRENT.with(|cell| {
                cell.replace(self.previous.take());
            });
        }
    }
}

pub fn enabled() -> bool {
    CURRENT.with(|cell| cell.borrow().is_some())
}

pub fn record_lock_wait(duration: Duration) {
    add(|c| c.lock_wait_ns = c.lock_wait_ns.saturating_add(nanos(duration)));
}

pub fn record_tree_lock_wait(duration: Duration) {
    add(|c| c.tree_lock_wait_ns = c.tree_lock_wait_ns.saturating_add(nanos(duration)));
}

pub fn record_wal_lock_wait(duration: Duration) {
    add(|c| c.wal_lock_wait_ns = c.wal_lock_wait_ns.saturating_add(nanos(duration)));
}

pub fn record_commit_lock_wait(duration: Duration) {
    add(|c| c.commit_lock_wait_ns = c.commit_lock_wait_ns.saturating_add(nanos(duration)));
}

pub fn record_wal_write(duration: Duration) {
    add(|c| c.wal_write_ns = c.wal_write_ns.saturating_add(nanos(duration)));
}

pub fn record_wal_sync(duration: Duration) {
    add(|c| c.wal_sync_ns = c.wal_sync_ns.saturating_add(nanos(duration)));
}

pub fn record_wal_durable_wait(duration: Duration) {
    add(|c| c.wal_durable_wait_ns = c.wal_durable_wait_ns.saturating_add(nanos(duration)));
}

pub fn record_point_get() {
    add(|c| c.point_gets = c.point_gets.saturating_add(1));
}

pub fn record_scan(entries: usize) {
    add(|c| {
        c.scans = c.scans.saturating_add(1);
        c.scan_entries = c.scan_entries.saturating_add(entries as u64);
    });
}

fn add(f: impl FnOnce(&mut QueryCounters)) {
    CURRENT.with(|cell| {
        if let Some(counters) = cell.borrow_mut().as_mut() {
            f(counters);
        }
    });
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}
