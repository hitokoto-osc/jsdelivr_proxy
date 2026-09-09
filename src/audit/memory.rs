use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use super::Record;

/// Fixed-size ring of the most recent records, newest first.
pub struct MemoryLog {
    entries: Mutex<VecDeque<Record>>,
    capacity: usize,
}

impl MemoryLog {
    pub fn new(capacity: usize) -> Self {
        MemoryLog {
            entries: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    /// A blocking `Mutex` in async code is fine here because the critical
    /// section is a `VecDeque` push with no `.await` inside it.
    pub fn append(&self, record: Record) {
        let mut entries = self.lock();
        entries.push_front(record);
        entries.truncate(self.capacity);
    }

    pub fn list(&self, limit: usize, offset: usize) -> Vec<Record> {
        self.lock()
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    }

    /// Nothing in the critical section can panic, so poisoning is unreachable;
    /// recovering from it anyway keeps a panic elsewhere from permanently
    /// disabling the audit log.
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Record>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::Actor;

    fn log_with(count: usize, capacity: usize) -> MemoryLog {
        let log = MemoryLog::new(capacity);
        for i in 0..count {
            log.append(Record::new(Actor::Admin, format!("action.{i}")));
        }
        log
    }

    #[test]
    fn newest_record_comes_first() {
        let log = log_with(3, 10);
        let records = log.list(10, 0);
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].action, "action.2");
        assert_eq!(records[2].action, "action.0");
    }

    #[test]
    fn the_oldest_records_are_dropped_at_capacity() {
        let log = log_with(5, 2);
        let records = log.list(10, 0);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].action, "action.4");
        assert_eq!(records[1].action, "action.3");
    }

    #[test]
    fn offset_and_limit_page_through_the_ring() {
        let log = log_with(5, 10);
        let page = log.list(2, 1);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].action, "action.3");
        assert_eq!(page[1].action, "action.2");
        assert!(log.list(2, 99).is_empty());
    }
}
