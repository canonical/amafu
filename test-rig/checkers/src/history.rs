use std::collections::{BTreeMap, HashSet};

pub type RegisterState = Option<(u64, u64)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterOperation {
    Read,
    Create { value: u64 },
    Write { expected_generation: u64, value: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterOutcome {
    Read(RegisterState),
    Created,
    AlreadyExists,
    Written { generation: u64 },
    Conflict,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub key: String,
    pub client: usize,
    pub called_at: i128,
    pub returned_at: Option<i128>,
    pub operation: RegisterOperation,
    pub outcome: RegisterOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotLinearizable {
    pub key: String,
    pub operations: usize,
}

fn step(
    state: RegisterState,
    operation: &RegisterOperation,
    outcome: &RegisterOutcome,
) -> Option<RegisterState> {
    let unknown = *outcome == RegisterOutcome::Unknown;
    match (operation, state) {
        (RegisterOperation::Read, current) => {
            (unknown || *outcome == RegisterOutcome::Read(current)).then_some(current)
        }
        (RegisterOperation::Create { value }, None) => {
            (unknown || *outcome == RegisterOutcome::Created).then_some(Some((1, *value)))
        }
        (RegisterOperation::Create { .. }, current @ Some(_)) => {
            (unknown || *outcome == RegisterOutcome::AlreadyExists).then_some(current)
        }
        (RegisterOperation::Write { expected_generation, value }, Some((generation, _)))
            if generation == *expected_generation =>
        {
            let written = RegisterOutcome::Written { generation: generation + 1 };
            (unknown || *outcome == written).then_some(Some((generation + 1, *value)))
        }
        (RegisterOperation::Write { .. }, current) => {
            (unknown || *outcome == RegisterOutcome::Conflict).then_some(current)
        }
    }
}

fn step_without_effect(state: RegisterState, outcome: &RegisterOutcome) -> Option<RegisterState> {
    (*outcome == RegisterOutcome::Unknown).then_some(state)
}

#[derive(Debug, Clone, Copy)]
enum Event {
    Call(usize),
    Return(usize),
}

struct Timeline {
    events: Vec<Event>,
    next: Vec<Option<usize>>,
    previous: Vec<Option<usize>>,
    head: Option<usize>,
    call_of: Vec<usize>,
    return_of: Vec<usize>,
}

impl Timeline {
    fn new(entries: &[&HistoryEntry]) -> Self {
        let mut timed: Vec<(i128, u8, usize, Event)> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            timed.push((entry.called_at, 0, index, Event::Call(index)));
            timed.push((entry.returned_at.unwrap_or(i128::MAX), 1, index, Event::Return(index)));
        }
        timed.sort_by_key(|(time, order, index, _)| (*time, *order, *index));
        let events: Vec<Event> = timed.into_iter().map(|(_, _, _, event)| event).collect();
        let count = events.len();
        let mut call_of = vec![0; entries.len()];
        let mut return_of = vec![0; entries.len()];
        for (position, event) in events.iter().enumerate() {
            match event {
                Event::Call(operation) => call_of[*operation] = position,
                Event::Return(operation) => return_of[*operation] = position,
            }
        }
        Self {
            events,
            next: (0..count).map(|position| (position + 1 < count).then_some(position + 1)).collect(),
            previous: (0..count).map(|position| position.checked_sub(1)).collect(),
            head: (count > 0).then_some(0),
            call_of,
            return_of,
        }
    }

    fn unlink(&mut self, position: usize) {
        let (before, after) = (self.previous[position], self.next[position]);
        match before {
            Some(before) => self.next[before] = after,
            None => self.head = after,
        }
        if let Some(after) = after {
            self.previous[after] = before;
        }
    }

    fn relink(&mut self, position: usize) {
        let (before, after) = (self.previous[position], self.next[position]);
        match before {
            Some(before) => self.next[before] = Some(position),
            None => self.head = Some(position),
        }
        if let Some(after) = after {
            self.previous[after] = Some(position);
        }
    }

    fn lift(&mut self, operation: usize) {
        self.unlink(self.call_of[operation]);
        self.unlink(self.return_of[operation]);
    }

    fn unlift(&mut self, operation: usize) {
        self.relink(self.return_of[operation]);
        self.relink(self.call_of[operation]);
    }
}

fn is_linearizable(entries: &[&HistoryEntry]) -> bool {
    let mut timeline = Timeline::new(entries);
    let mut linearized = vec![false; entries.len()];
    let mut seen: HashSet<(Vec<bool>, RegisterState)> = HashSet::new();
    let mut stack: Vec<(usize, RegisterState)> = Vec::new();
    let mut state: RegisterState = None;
    let mut cursor = timeline.head;
    while let Some(position) = cursor {
        match timeline.events[position] {
            Event::Call(operation) => {
                let entry = entries[operation];
                let candidates = [
                    step(state, &entry.operation, &entry.outcome),
                    step_without_effect(state, &entry.outcome),
                ];
                let mut advanced = false;
                for next_state in candidates.into_iter().flatten() {
                    linearized[operation] = true;
                    if seen.insert((linearized.clone(), next_state)) {
                        stack.push((operation, state));
                        state = next_state;
                        timeline.lift(operation);
                        cursor = timeline.head;
                        advanced = true;
                        break;
                    }
                    linearized[operation] = false;
                }
                if !advanced {
                    cursor = timeline.next[position];
                }
            }
            Event::Return(_) => {
                let Some((operation, previous_state)) = stack.pop() else {
                    return false;
                };
                state = previous_state;
                linearized[operation] = false;
                timeline.unlift(operation);
                cursor = timeline.next[timeline.call_of[operation]];
            }
        }
    }
    true
}

pub fn check_linearizable(history: &[HistoryEntry]) -> Result<(), NotLinearizable> {
    let mut by_key: BTreeMap<&str, Vec<&HistoryEntry>> = BTreeMap::new();
    for entry in history {
        by_key.entry(entry.key.as_str()).or_default().push(entry);
    }
    for (key, entries) in by_key {
        if !is_linearizable(&entries) {
            return Err(NotLinearizable { key: key.to_owned(), operations: entries.len() });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        client: usize,
        called_at: i128,
        returned_at: Option<i128>,
        operation: RegisterOperation,
        outcome: RegisterOutcome,
    ) -> HistoryEntry {
        HistoryEntry { key: String::from("register"), client, called_at, returned_at, operation, outcome }
    }

    fn create(client: usize, from: i128, to: i128) -> HistoryEntry {
        entry(client, from, Some(to), RegisterOperation::Create { value: 0 }, RegisterOutcome::Created)
    }

    fn read(client: usize, from: i128, to: i128, state: RegisterState) -> HistoryEntry {
        entry(client, from, Some(to), RegisterOperation::Read, RegisterOutcome::Read(state))
    }

    fn write(
        client: usize,
        from: i128,
        to: Option<i128>,
        expected: u64,
        value: u64,
        outcome: RegisterOutcome,
    ) -> HistoryEntry {
        entry(client, from, to, RegisterOperation::Write { expected_generation: expected, value }, outcome)
    }

    #[test]
    fn sequential_operations_are_linearizable() {
        let history = vec![
            create(0, 0, 1),
            write(0, 2, Some(3), 1, 7, RegisterOutcome::Written { generation: 2 }),
            read(1, 4, 5, Some((2, 7))),
            write(1, 6, Some(7), 1, 9, RegisterOutcome::Conflict),
        ];
        assert_eq!(check_linearizable(&history), Ok(()));
    }

    #[test]
    fn a_read_of_a_value_before_it_was_written_is_not_linearizable() {
        let history = vec![
            create(0, 0, 1),
            read(1, 2, 3, Some((2, 7))),
            write(0, 4, Some(5), 1, 7, RegisterOutcome::Written { generation: 2 }),
        ];
        assert!(check_linearizable(&history).is_err());
    }

    #[test]
    fn overlapping_operations_may_take_effect_in_either_order() {
        let history = vec![
            create(0, 0, 1),
            write(0, 2, Some(10), 1, 7, RegisterOutcome::Written { generation: 2 }),
            read(1, 3, 4, Some((2, 7))),
            read(2, 5, 6, Some((2, 7))),
        ];
        assert_eq!(check_linearizable(&history), Ok(()));
    }

    #[test]
    fn two_writes_cannot_both_win_from_the_same_generation() {
        let history = vec![
            create(0, 0, 1),
            write(0, 2, Some(5), 1, 7, RegisterOutcome::Written { generation: 2 }),
            write(1, 2, Some(5), 1, 8, RegisterOutcome::Written { generation: 2 }),
        ];
        assert!(check_linearizable(&history).is_err());
    }

    #[test]
    fn an_unanswered_write_may_or_may_not_have_happened() {
        let lost = vec![
            create(0, 0, 1),
            write(0, 2, None, 1, 7, RegisterOutcome::Unknown),
            read(1, 5, 6, Some((1, 0))),
        ];
        assert_eq!(check_linearizable(&lost), Ok(()));
        let applied = vec![
            create(0, 0, 1),
            write(0, 2, None, 1, 7, RegisterOutcome::Unknown),
            read(1, 5, 6, Some((2, 7))),
        ];
        assert_eq!(check_linearizable(&applied), Ok(()));
        let impossible = vec![
            create(0, 0, 1),
            write(0, 2, None, 1, 7, RegisterOutcome::Unknown),
            read(1, 5, 6, Some((2, 8))),
        ];
        assert!(check_linearizable(&impossible).is_err());
    }

    #[test]
    fn a_stale_read_after_a_completed_write_is_not_linearizable() {
        let history = vec![
            create(0, 0, 1),
            write(0, 2, Some(3), 1, 7, RegisterOutcome::Written { generation: 2 }),
            read(1, 4, 5, Some((1, 0))),
        ];
        assert!(check_linearizable(&history).is_err());
    }
}
