//! Seeded property tests. Each case builds a random delegation tree and applies a
//! random sequence of reserve, commit, release and drop operations, checking the
//! store against a plain reference model after every step. A failing case prints
//! its seed so it can be replayed.
mod support;

use foundry_budget::{Amount, BudgetError, BudgetId, Reservation, WallClock};
use support::{Rng, manual_store, xts};

const CASES: u64 = 300;
const STEPS: usize = 250;

struct ModelBudget {
    parent: Option<usize>,
    ceiling: Option<u128>,
    committed: u128,
    reserved: u128,
    overrun: bool,
    /// How far settlements in this budget's subtree exceeded their reservations.
    excess: u128,
}

struct Model {
    budgets: Vec<ModelBudget>,
}

impl Model {
    fn chain(&self, mut index: usize) -> Vec<usize> {
        let mut chain = vec![index];
        while let Some(parent) = self.budgets[index].parent {
            chain.push(parent);
            index = parent;
        }
        chain
    }

    fn remaining(&self, index: usize) -> Option<u128> {
        self.chain(index)
            .into_iter()
            .filter_map(|member| {
                let budget = &self.budgets[member];
                budget
                    .ceiling
                    .map(|ceiling| ceiling.saturating_sub(budget.committed + budget.reserved))
            })
            .min()
    }

    /// The first chain member that refuses `atoms`, nearest first.
    fn refusal(&self, index: usize, atoms: u128) -> Option<usize> {
        self.chain(index).into_iter().find(|&member| {
            let budget = &self.budgets[member];
            budget.overrun
                || budget
                    .ceiling
                    .is_some_and(|ceiling| budget.committed + budget.reserved + atoms > ceiling)
        })
    }

    fn reserve(&mut self, index: usize, atoms: u128) {
        for member in self.chain(index) {
            self.budgets[member].reserved += atoms;
        }
    }

    fn settle(&mut self, index: usize, reserved: u128, actual: u128) {
        for member in self.chain(index) {
            let budget = &mut self.budgets[member];
            budget.reserved -= reserved;
            budget.committed += actual;
            // An overrun stops the whole chain the settlement belongs to.
            budget.overrun |= actual > reserved;
            budget.excess += actual.saturating_sub(reserved);
        }
    }
}

fn atoms(value: u128) -> Amount {
    Amount::from_atoms(xts(), value).unwrap()
}

fn run_case(seed: u64) {
    let mut rng = Rng::new(seed);
    let (store, _) = manual_store();
    let mut model = Model {
        budgets: Vec::new(),
    };
    let mut ids: Vec<BudgetId> = Vec::new();

    let root_ceiling = (rng.below(4) != 0).then(|| u128::from(100 + rng.below(2_000)));
    ids.push(
        store
            .create(xts(), root_ceiling.map(atoms), WallClock::NONE)
            .unwrap(),
    );
    model.budgets.push(ModelBudget {
        parent: None,
        ceiling: root_ceiling,
        committed: 0,
        reserved: 0,
        overrun: false,
        excess: 0,
    });
    for _ in 0..rng.below(6) {
        let parent = rng.below(ids.len() as u64) as usize;
        let ceiling = (rng.below(3) != 0).then(|| u128::from(rng.below(2_500)));
        let result = store.delegate(ids[parent], ceiling.map(atoms), WallClock::NONE);
        let widens = ceiling
            .zip(model.remaining(parent))
            .is_some_and(|(ceiling, remaining)| ceiling > remaining);
        if widens {
            let error = result.unwrap_err();
            assert_eq!(
                error,
                BudgetError::InvalidRequest {
                    field: "child_ceiling"
                }
            );
            continue;
        }
        ids.push(result.unwrap());
        model.budgets.push(ModelBudget {
            parent: Some(parent),
            ceiling,
            committed: 0,
            reserved: 0,
            overrun: false,
            excess: 0,
        });
    }

    let mut open: Vec<(usize, Reservation)> = Vec::new();
    for _ in 0..STEPS {
        match rng.below(5) {
            0 | 1 => {
                let index = rng.below(ids.len() as u64) as usize;
                let amount = u128::from(rng.below(300));
                let result = store.reserve(ids[index], atoms(amount));
                match (model.refusal(index, amount), result) {
                    (None, Ok(reservation)) => {
                        model.reserve(index, amount);
                        open.push((index, reservation));
                    }
                    (Some(refuser), Err(BudgetError::BudgetExhausted { budget, .. })) => {
                        assert_eq!(budget, ids[refuser]);
                    }
                    (expected, actual) => {
                        panic!("model refusal {expected:?}, store returned {actual:?}")
                    }
                }
            }
            2 | 3 if !open.is_empty() => {
                let (index, mut reservation) =
                    open.swap_remove(rng.below(open.len() as u64) as usize);
                let reserved = reservation.amount().atoms();
                // An occasional commit exceeds its reservation. Overruns stop the
                // whole chain, so they stay rare to keep later steps meaningful.
                let actual = if rng.below(40) == 0 {
                    reserved + 1 + u128::from(rng.below(50))
                } else {
                    u128::from(rng.below(reserved as u64 + 1))
                };
                let settlement = reservation.commit(atoms(actual)).unwrap();
                model.settle(index, reserved, actual);
                assert_eq!(settlement.charged.atoms(), actual);
                assert_eq!(settlement.released.atoms(), reserved.saturating_sub(actual));
                assert_eq!(settlement.overrun, actual > reserved);
            }
            4 if !open.is_empty() => {
                let (index, reservation) = open.swap_remove(rng.below(open.len() as u64) as usize);
                let reserved = reservation.amount().atoms();
                // An abandoned reservation is charged in full.
                drop(reservation);
                model.settle(index, reserved, reserved);
            }
            _ => {}
        }
        check(&store, &ids, &model);
    }
    drop(open);
    for (index, budget) in ids.iter().enumerate() {
        assert_eq!(
            store.snapshot(*budget).unwrap().reserved.atoms(),
            0,
            "budget {index}"
        );
    }
}

fn check(store: &foundry_budget::BudgetStore, ids: &[BudgetId], model: &Model) {
    for (index, budget) in ids.iter().enumerate() {
        let snapshot = store.snapshot(*budget).unwrap();
        let expected = &model.budgets[index];
        assert_eq!(
            snapshot.committed.atoms(),
            expected.committed,
            "committed {index}"
        );
        assert_eq!(
            snapshot.reserved.atoms(),
            expected.reserved,
            "reserved {index}"
        );
        let chain_overrun = model
            .chain(index)
            .into_iter()
            .any(|member| model.budgets[member].overrun);
        assert_eq!(snapshot.overrun, chain_overrun, "overrun {index}");
        assert_eq!(
            snapshot.remaining.map(|amount| amount.atoms()),
            model.remaining(index)
        );
        // Committed spend plus open reservations never exceeds the ceiling by
        // more than the subtree's overruns exceeded their reservations. Checked
        // against the store's own figures, not only the model's.
        if let Some(ceiling) = expected.ceiling {
            let used = snapshot.committed.atoms() + snapshot.reserved.atoms();
            assert!(used <= ceiling + expected.excess, "budget {index}");
        }
    }
}

#[test]
fn random_operation_sequences_match_the_reference_model() {
    for seed in 0..CASES {
        let outcome = std::panic::catch_unwind(|| run_case(seed));
        assert!(outcome.is_ok(), "property case failed for seed {seed}");
    }
}

#[test]
fn a_child_never_spends_past_any_ancestor_without_overrun() {
    // Without overruns the parent's committed spend is exactly the sum of its own
    // and its children's commits, and never exceeds its ceiling.
    for seed in 0..CASES {
        let mut rng = Rng::new(seed ^ 0xA5A5);
        let (store, _) = manual_store();
        let ceiling = 500 + u128::from(rng.below(1_000));
        let parent = store
            .create(xts(), Some(atoms(ceiling)), WallClock::NONE)
            .unwrap();
        let children: Vec<BudgetId> = (0..1 + rng.below(4))
            .map(|_| {
                let narrowed = (rng.below(2) == 0).then(|| atoms(u128::from(rng.below(500))));
                store.delegate(parent, narrowed, WallClock::NONE).unwrap()
            })
            .collect();
        let mut charged = 0u128;
        for _ in 0..200 {
            let child = children[rng.below(children.len() as u64) as usize];
            let maximum = u128::from(1 + rng.below(120));
            if let Ok(mut reservation) = store.reserve(child, atoms(maximum)) {
                let actual = u128::from(rng.below(maximum as u64 + 1));
                reservation.commit(atoms(actual)).unwrap();
                charged += actual;
            }
        }
        let snapshot = store.snapshot(parent).unwrap();
        assert_eq!(snapshot.committed.atoms(), charged, "seed {seed}");
        assert!(charged <= ceiling, "seed {seed}");
        assert!(!snapshot.overrun, "seed {seed}");
    }
}
