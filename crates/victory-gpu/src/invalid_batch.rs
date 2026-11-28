//! Revalidate deferred invalid slots while the caller holds the heap lock.
//! A slot made valid since it was queued must be retried, never cleared.
pub(crate) fn flush_locked(
    pending: &[(u32, u64)],
    made: &mut [u64],
    mut valid_now: impl FnMut(u32, u64) -> bool,
    mut clear: impl FnMut(&mut [u64], u32),
) -> Vec<u32> {
    let mut retry = Vec::new();
    for &(index, handle) in pending {
        if index as usize >= made.len() { continue; }
        if valid_now(index, handle) {
            retry.push(index);
        } else {
            clear(made, index);
        }
    }
    retry
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex, mpsc};

    #[test]
    fn a_view_published_after_queueing_is_preserved_and_retried() {
        let mut made = [11, 22];
        let retry = flush_locked(&[(0, 100), (1, 101)], &mut made,
            |index, _| index == 1,
            |made, index| made[index as usize] = 0);
        assert_eq!(made, [0, 22]);
        assert_eq!(retry, vec![1]);
    }

    #[test]
    fn concurrent_publication_and_invalidation_use_the_same_lock() {
        // Models the same made-lock boundary as HostHeap: a writer publishes
        // a new generation before the drain can revalidate queued old data.
        let heap = Arc::new(Mutex::new((vec![0u64], false)));
        let (published, ready) = mpsc::channel();
        let writer = {
            let heap = heap.clone();
            std::thread::spawn(move || {
                let mut heap = heap.lock().unwrap();
                heap.0[0] = 77;
                heap.1 = true;
                published.send(()).unwrap();
            })
        };
        ready.recv().unwrap();
        let mut heap = heap.lock().unwrap();
        let valid = heap.1;
        let retry = flush_locked(&[(0, 100)], &mut heap.0, |_, _| valid,
            |made, index| made[index as usize] = 0);
        assert_eq!(heap.0, vec![77]);
        assert_eq!(retry, vec![0]);
        // A later resource release must still clear that published slot.
        heap.1 = false;
        let retry = flush_locked(&[(0, 100)], &mut heap.0, |_, _| false,
            |made, index| made[index as usize] = 0);
        assert!(retry.is_empty());
        assert_eq!(heap.0, vec![0]);
        drop(heap);
        writer.join().unwrap();
    }
}
