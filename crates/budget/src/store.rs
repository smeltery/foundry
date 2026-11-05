//! The crate's own store: an in-memory ledger behind one mutex. It is not part of
//! the main SQLite migration chain and does not persist; open reservations and
//! committed spend last only as long as the process.
//!
//! Every reservation and settlement walks the budget's delegation chain (the
//! budget, its parent, and so on to the root) under a single lock, so concurrent
//! reservers are serialized and a child can never spend past any ancestor.
use crate::amount::{Amount, CostUnit};
use crate::clock::{Clock, WallClock};
use crate::error::{BudgetError, ExhaustedCause};
use crate::reservation::{Reservation, Settlement};
use crate::termination::Termination;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BudgetId(u64);

impl BudgetId {
    pub fn get(&self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReservationId(u64);

/// Whether a budget's effective deadline (the earliest in its chain) has passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeadlineState {
    Unbounded,
    Remaining { ms: u64 },
    Exceeded,
}

impl DeadlineState {
    /// The settlement for the running attempt once the deadline has passed.
    pub fn termination(&self) -> Option<Termination> {
        matches!(self, Self::Exceeded).then(Termination::deadline_exceeded)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub parent: Option<BudgetId>,
    pub ceiling: Option<Amount>,
    pub committed: Amount,
    pub reserved: Amount,
    /// The most a new reservation could take now, across the whole chain;
    /// `None` when no budget in the chain has a ceiling.
    pub remaining: Option<Amount>,
    pub overrun: bool,
    pub deadline: DeadlineState,
}

struct Ledger {
    unit: CostUnit,
    ceiling: Option<u128>,
    committed: u128,
    reserved: u128,
    overrun: bool,
    parent: Option<BudgetId>,
    deadline_at_ms: Option<u64>,
}

impl Ledger {
    fn remaining(&self) -> Option<u128> {
        self.ceiling
            .map(|ceiling| ceiling.saturating_sub(self.committed.saturating_add(self.reserved)))
    }
}

struct OpenReservation {
    budget: BudgetId,
    atoms: u128,
}

#[derive(Default)]
struct State {
    last_id: u64,
    budgets: HashMap<BudgetId, Ledger>,
    reservations: HashMap<ReservationId, OpenReservation>,
}

impl State {
    fn next_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }

    fn ledger(&self, id: BudgetId) -> Result<&Ledger, BudgetError> {
        self.budgets
            .get(&id)
            .ok_or(BudgetError::NotFound { what: "budget" })
    }

    /// Chain members are only ever looked up after `chain` proved they exist,
    /// and budgets are never removed.
    fn ledger_mut(&mut self, id: BudgetId) -> &mut Ledger {
        self.budgets.get_mut(&id).expect("chain members exist")
    }

    /// The budget followed by each ancestor up to the root.
    fn chain(&self, id: BudgetId) -> Result<Vec<BudgetId>, BudgetError> {
        let mut chain = Vec::new();
        let mut next = Some(id);
        while let Some(current) = next {
            next = self.ledger(current)?.parent;
            chain.push(current);
        }
        Ok(chain)
    }

    fn remaining(&self, chain: &[BudgetId]) -> Option<u128> {
        chain
            .iter()
            .filter_map(|id| self.budgets.get(id).and_then(Ledger::remaining))
            .min()
    }

    fn deadline(&self, chain: &[BudgetId], now_ms: u64) -> DeadlineState {
        let earliest = chain
            .iter()
            .filter_map(|id| {
                self.budgets
                    .get(id)
                    .and_then(|ledger| ledger.deadline_at_ms)
            })
            .min();
        match earliest {
            None => DeadlineState::Unbounded,
            Some(at) if now_ms >= at => DeadlineState::Exceeded,
            Some(at) => DeadlineState::Remaining { ms: at - now_ms },
        }
    }

    fn dispatch_allowed(&self, chain: &[BudgetId], now_ms: u64) -> bool {
        chain
            .iter()
            .all(|id| self.budgets.get(id).is_some_and(|ledger| !ledger.overrun))
            && self.deadline(chain, now_ms) != DeadlineState::Exceeded
    }

    /// Checks every chain member in order, nearest first, and returns the first
    /// refusal: deadline, then overrun, then ceiling.
    fn admit(&self, chain: &[BudgetId], amount: Amount, now_ms: u64) -> Result<(), BudgetError> {
        for &budget in chain {
            let ledger = self.ledger(budget)?;
            let refuse = |cause| Err(BudgetError::BudgetExhausted { budget, cause });
            if ledger.deadline_at_ms.is_some_and(|at| now_ms >= at) {
                return refuse(ExhaustedCause::DeadlineExceeded);
            }
            if ledger.overrun {
                return refuse(ExhaustedCause::Overrun);
            }
            let total = ledger
                .committed
                .checked_add(ledger.reserved)
                .and_then(|open| open.checked_add(amount.atoms()))
                .ok_or(BudgetError::invalid("amount"))?;
            Amount::from_atoms(amount.unit(), total)?;
            if ledger.ceiling.is_some_and(|ceiling| total > ceiling) {
                return refuse(ExhaustedCause::Ceiling);
            }
        }
        Ok(())
    }
}

struct Shared {
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

/// Cloning shares the same ledger.
#[derive(Clone)]
pub struct BudgetStore {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for BudgetStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BudgetStore")
            .finish_non_exhaustive()
    }
}

impl BudgetStore {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            shared: Arc::new(Shared {
                clock,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// Opens a root budget at execution admission. A `None` ceiling sets no local
    /// limit and keeps the account's own limits.
    pub fn open(
        &self,
        unit: CostUnit,
        ceiling: Option<Amount>,
        wall_clock: WallClock,
    ) -> Result<BudgetId, BudgetError> {
        let now_ms = self.shared.clock.now_ms();
        let mut state = self.lock()?;
        Self::insert(
            &mut state,
            unit,
            ceiling,
            None,
            wall_clock.deadline_from(now_ms),
        )
    }

    /// Opens a child budget for a delegated session or automatic continuation.
    /// The child shares the parent's spend: every child reservation also counts
    /// against each ancestor. A child ceiling may narrow but never widen the
    /// chain, so it must fit within the parent chain's current remainder.
    pub fn delegate(
        &self,
        parent: BudgetId,
        ceiling: Option<Amount>,
        wall_clock: WallClock,
    ) -> Result<BudgetId, BudgetError> {
        let now_ms = self.shared.clock.now_ms();
        let mut state = self.lock()?;
        let chain = state.chain(parent)?;
        let unit = state.ledger(parent)?.unit;
        if let (Some(ceiling), Some(remaining)) = (ceiling, state.remaining(&chain))
            && ceiling.atoms() > remaining
        {
            return Err(BudgetError::invalid("child_ceiling"));
        }
        // The child's own deadline is checked alongside every ancestor's, so the
        // effective deadline is the earliest in the chain.
        let deadline_at_ms = wall_clock.deadline_from(now_ms);
        Self::insert(&mut state, unit, ceiling, Some(parent), deadline_at_ms)
    }

    /// Atomically reserves a call's maximum cost before dispatch. It succeeds only
    /// when committed spend plus open reservations plus `amount` stays within the
    /// ceiling of the budget and of every ancestor.
    pub fn reserve(&self, budget: BudgetId, amount: Amount) -> Result<Reservation, BudgetError> {
        let now_ms = self.shared.clock.now_ms();
        let mut state = self.lock()?;
        if state.ledger(budget)?.unit != amount.unit() {
            return Err(BudgetError::invalid("unit"));
        }
        let chain = state.chain(budget)?;
        state.admit(&chain, amount, now_ms)?;
        for &member in &chain {
            state.ledger_mut(member).reserved += amount.atoms();
        }
        let id = ReservationId(state.next_id());
        let open = OpenReservation {
            budget,
            atoms: amount.atoms(),
        };
        state.reservations.insert(id, open);
        Ok(Reservation::new(self.clone(), id, budget, amount))
    }

    pub fn snapshot(&self, budget: BudgetId) -> Result<BudgetSnapshot, BudgetError> {
        let now_ms = self.shared.clock.now_ms();
        let state = self.lock()?;
        let chain = state.chain(budget)?;
        let ledger = state.ledger(budget)?;
        let amount = |atoms| Amount::from_atoms(ledger.unit, atoms);
        Ok(BudgetSnapshot {
            parent: ledger.parent,
            ceiling: ledger.ceiling.map(amount).transpose()?,
            committed: amount(ledger.committed)?,
            reserved: amount(ledger.reserved)?,
            remaining: state.remaining(&chain).map(amount).transpose()?,
            overrun: ledger.overrun,
            deadline: state.deadline(&chain, now_ms),
        })
    }

    /// Commits `actual` and releases the rest of the reservation along the chain.
    /// An actual cost above the reservation is still committed in full, marks the
    /// budget overrun and refuses further dispatch; an ancestor pushed past its
    /// ceiling is marked overrun too.
    pub(crate) fn settle(
        &self,
        id: ReservationId,
        actual: Amount,
    ) -> Result<Settlement, BudgetError> {
        let now_ms = self.shared.clock.now_ms();
        let mut state = self.lock()?;
        let open = state.reservations.get(&id).ok_or(BudgetError::NotFound {
            what: "reservation",
        })?;
        let (budget, reserved_atoms) = (open.budget, open.atoms);
        if state.ledger(budget)?.unit != actual.unit() {
            return Err(BudgetError::invalid("unit"));
        }
        let chain = state.chain(budget)?;
        for &member in &chain {
            let committed = state.ledger(member)?.committed;
            let total = committed
                .checked_add(actual.atoms())
                .ok_or(BudgetError::invalid("amount"))?;
            Amount::from_atoms(actual.unit(), total)?;
        }
        state.reservations.remove(&id);
        let overrun = actual.atoms() > reserved_atoms;
        for &member in &chain {
            let ledger = state.ledger_mut(member);
            ledger.reserved -= reserved_atoms;
            ledger.committed += actual.atoms();
            let over_ceiling = ledger
                .ceiling
                .is_some_and(|ceiling| ledger.committed > ceiling);
            ledger.overrun |= over_ceiling || (overrun && member == budget);
        }
        let unit = actual.unit();
        Ok(Settlement {
            charged: actual,
            released: Amount::from_atoms(unit, reserved_atoms.saturating_sub(actual.atoms()))?,
            committed: Amount::from_atoms(unit, state.ledger(budget)?.committed)?,
            overrun,
            dispatch_allowed: state.dispatch_allowed(&chain, now_ms),
        })
    }

    fn insert(
        state: &mut State,
        unit: CostUnit,
        ceiling: Option<Amount>,
        parent: Option<BudgetId>,
        deadline_at_ms: Option<u64>,
    ) -> Result<BudgetId, BudgetError> {
        if ceiling.is_some_and(|ceiling| ceiling.unit() != unit) {
            return Err(BudgetError::invalid("unit"));
        }
        let id = BudgetId(state.next_id());
        let ledger = Ledger {
            unit,
            ceiling: ceiling.map(|ceiling| ceiling.atoms()),
            committed: 0,
            reserved: 0,
            overrun: false,
            parent,
            deadline_at_ms,
        };
        state.budgets.insert(id, ledger);
        Ok(id)
    }

    /// Ledger updates are validated before they are applied, so a panic cannot
    /// leave one half-applied. A poisoned lock is still reported, not ignored.
    fn lock(&self) -> Result<MutexGuard<'_, State>, BudgetError> {
        self.shared
            .state
            .lock()
            .map_err(|_| BudgetError::Unavailable)
    }
}
