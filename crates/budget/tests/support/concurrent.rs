//! Shared helpers for the concurrency tests: file-backed stores and a watcher
//! thread that checks ceilings while workers run.
use foundry_budget::{Amount, BudgetId, BudgetStore, SystemClock};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use super::xts;

pub fn open(path: &Path) -> BudgetStore {
    BudgetStore::open(path, Arc::new(SystemClock)).unwrap()
}

pub fn temp_store() -> (tempfile::TempDir, BudgetStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory.path().join("budget.sqlite"));
    (directory, store)
}

/// `count` separate handles, each with its own connection, on one new file.
pub fn temp_handles(count: usize) -> (tempfile::TempDir, Vec<BudgetStore>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    let handles = (0..count).map(|_| open(&path)).collect();
    (directory, handles)
}

pub fn atoms(value: u64) -> Amount {
    Amount::from_atoms(xts(), u128::from(value)).unwrap()
}

/// Samples `budgets` until `done`, asserting no ceiling is ever exceeded by
/// committed spend plus open reservations.
pub fn watch(store: &BudgetStore, budgets: &[(BudgetId, u128)], done: &AtomicBool) -> u64 {
    let mut samples = 0;
    while !done.load(Ordering::SeqCst) {
        for &(budget, ceiling) in budgets {
            let snapshot = store.snapshot(budget).unwrap();
            let used = snapshot.committed.atoms() + snapshot.reserved.atoms();
            assert!(
                used <= ceiling,
                "budget {budget:?} used {used} of {ceiling}"
            );
            samples += 1;
        }
    }
    samples
}

/// Runs `workers` while a watcher checks `budgets`, then stops the watcher.
pub fn with_watcher(store: &BudgetStore, budgets: &[(BudgetId, u128)], workers: impl FnOnce()) {
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        let watcher = scope.spawn(|| watch(store, budgets, &done));
        workers();
        done.store(true, Ordering::SeqCst);
        assert!(watcher.join().unwrap() > 0);
    });
}
