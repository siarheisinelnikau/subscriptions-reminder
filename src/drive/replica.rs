use crate::model::{Database, Subscription};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

type Clock = BTreeMap<Uuid, u64>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    pub clock: Clock,
    pub value: Option<Subscription>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Replica {
    pub format_version: u32,
    pub records: BTreeMap<Uuid, Vec<Revision>>,
}
impl Default for Replica {
    fn default() -> Self {
        Self {
            format_version: 1,
            records: BTreeMap::new(),
        }
    }
}

fn dominates(left: &Clock, right: &Clock) -> bool {
    left != right
        && right
            .iter()
            .all(|(id, count)| left.get(id).copied().unwrap_or(0) >= *count)
}
fn join_clock(clock: &mut Clock, other: &Clock) {
    for (id, count) in other {
        clock
            .entry(*id)
            .and_modify(|n| *n = (*n).max(*count))
            .or_insert(*count);
    }
}
fn normalize(values: Vec<Revision>) -> Result<Vec<Revision>, String> {
    let mut unique = BTreeMap::<Clock, Revision>::new();
    for value in values {
        if let Some(old) = unique.get(&value.clock) {
            if old.value != value.value {
                return Err("Cloud data contains conflicting values for the same revision. Nothing was overwritten.".into());
            }
        } else {
            unique.insert(value.clock.clone(), value);
        }
    }
    let values: Vec<_> = unique.into_values().collect();
    Ok(values
        .iter()
        .filter(|item| {
            !values
                .iter()
                .any(|other| dominates(&other.clock, &item.clock))
        })
        .cloned()
        .collect())
}

impl Replica {
    pub fn validate(&self) -> Result<(), String> {
        if self.format_version != 1 {
            return Err(
                "The cloud data format is newer than this application. Update before syncing."
                    .into(),
            );
        }
        if self.records.len() > 100_000 {
            return Err("The cloud database contains too many records.".into());
        }
        for (id, revisions) in &self.records {
            if revisions.is_empty() || revisions.len() > 256 {
                return Err("The cloud database has an invalid revision list.".into());
            }
            for revision in revisions {
                if revision.clock.is_empty()
                    || revision.clock.len() > 1024
                    || revision.clock.values().any(|n| *n == 0)
                {
                    return Err("The cloud database has an invalid revision clock.".into());
                }
                if let Some(sub) = &revision.value {
                    sub.validate()?;
                    if sub.id != *id {
                        return Err("The cloud database has mismatched subscription IDs.".into());
                    }
                }
            }
            normalize(revisions.clone())?;
        }
        Ok(())
    }
    pub fn merge(&mut self, other: &Self) -> Result<(), String> {
        other.validate()?;
        let mut merged = self.clone();
        for (id, values) in &other.records {
            let mut both = merged.records.remove(id).unwrap_or_default();
            both.extend(values.iter().cloned());
            merged.records.insert(*id, normalize(both)?);
        }
        merged.validate()?;
        *self = merged;
        Ok(())
    }
    fn update(
        &mut self,
        id: Uuid,
        value: Option<Subscription>,
        previous: Option<&Subscription>,
        device: Uuid,
        resolve: bool,
    ) -> Result<(), String> {
        let versions = self.records.get(&id).cloned().unwrap_or_default();
        let conflicted = distinct_values(&versions).len() > 1;
        let mut clock = Clock::new();
        for revision in &versions {
            // A regular edit during a conflict supersedes the version visible locally,
            // preserving other branches until the user explicitly resolves them.
            if resolve || !conflicted || revision.value.as_ref() == previous {
                join_clock(&mut clock, &revision.clock);
            }
        }
        let next = versions
            .iter()
            .filter_map(|v| v.clock.get(&device))
            .copied()
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("The sync revision counter is exhausted.")?;
        clock.insert(device, next);
        let mut updated = versions;
        updated.push(Revision { clock, value });
        self.records.insert(id, normalize(updated)?);
        Ok(())
    }
    pub fn observe_local(
        &mut self,
        previous: &Database,
        current: &Database,
        device: Uuid,
    ) -> Result<bool, String> {
        current.validate()?;
        let before: BTreeMap<_, _> = previous.subscriptions.iter().map(|s| (s.id, s)).collect();
        let after: BTreeMap<_, _> = current.subscriptions.iter().map(|s| (s.id, s)).collect();
        let ids: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
        let mut changed = false;
        for id in ids {
            let old = before.get(&id).copied();
            let new = after.get(&id).copied();
            if old != new {
                self.update(id, new.cloned(), old, device, false)?;
                changed = true;
            }
        }
        Ok(changed)
    }
    pub fn conflicts(&self) -> Vec<Conflict> {
        self.records
            .iter()
            .filter_map(|(id, revisions)| {
                let choices = distinct_values(revisions);
                (choices.len() > 1).then_some(Conflict { id: *id, choices })
            })
            .collect()
    }
    pub fn materialize(&self, current: &Database) -> Database {
        let local: BTreeMap<_, _> = current.subscriptions.iter().map(|s| (s.id, s)).collect();
        let mut db = Database::default();
        for (id, revisions) in &self.records {
            let choices = distinct_values(revisions);
            let previous = local.get(id).copied();
            let selected = if choices.len() > 1 {
                choices
                    .iter()
                    .find(|v| v.as_ref() == previous)
                    .unwrap_or(&choices[0])
            } else {
                &choices[0]
            };
            if let Some(sub) = selected {
                db.subscriptions.push(sub.clone());
            }
        }
        db.subscriptions.sort_by_key(|s| s.id);
        db
    }
    pub fn resolve(
        &mut self,
        conflict: &Conflict,
        choice: Resolution,
        device: Uuid,
    ) -> Result<(), String> {
        let current = self
            .conflicts()
            .into_iter()
            .find(|c| c.id == conflict.id)
            .ok_or("This conflict has already been resolved.")?;
        if current != *conflict {
            return Err(
                "This conflict changed during synchronization. Review its latest versions.".into(),
            );
        }
        match choice {
            Resolution::Choose(index) => {
                let value = conflict
                    .choices
                    .get(index)
                    .ok_or("Choose a valid conflict version.")?
                    .clone();
                self.update(conflict.id, value, None, device, true)?;
            }
            Resolution::KeepAll => {
                let mut values = conflict.choices.iter().filter_map(Clone::clone);
                let first = values
                    .next()
                    .ok_or("There are no subscription versions to keep.")?;
                self.update(conflict.id, Some(first), None, device, true)?;
                for mut value in values {
                    value.id = Uuid::new_v4();
                    value.name = format!(
                        "{} (conflict copy)",
                        value.name.chars().take(134).collect::<String>()
                    );
                    self.update(value.id, Some(value), None, device, true)?;
                }
            }
        }
        Ok(())
    }
}

fn distinct_values(revisions: &[Revision]) -> Vec<Option<Subscription>> {
    let mut values = vec![];
    for revision in revisions {
        if !values.contains(&revision.value) {
            values.push(revision.value.clone());
        }
    }
    values
}
#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub id: Uuid,
    pub choices: Vec<Option<Subscription>>,
}
#[derive(Clone, Copy)]
pub enum Resolution {
    Choose(usize),
    KeepAll,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Database {
        let mut sub = Subscription::new("2026-11-01".parse().unwrap());
        sub.name = "Cloud".into();
        Database {
            schema_version: 1,
            subscriptions: vec![sub],
        }
    }
    #[test]
    fn independent_changes_converge_and_merge_is_idempotent() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let original = db();
        let mut left = Replica::default();
        left.observe_local(&Database::default(), &original, a)
            .unwrap();
        let mut right = left.clone();
        let mut changed = original.clone();
        changed.subscriptions[0].notes = "Updated elsewhere".into();
        right.observe_local(&original, &changed, b).unwrap();
        let extra = db();
        let mut local = original.clone();
        local.subscriptions.extend(extra.subscriptions);
        left.observe_local(&original, &local, a).unwrap();
        let mut forward = left.clone();
        forward.merge(&right).unwrap();
        let mut reverse = right.clone();
        reverse.merge(&left).unwrap();
        assert_eq!(forward, reverse);
        forward.merge(&reverse).unwrap();
        assert_eq!(forward, reverse);
        assert!(forward.conflicts().is_empty());
        assert_eq!(forward.materialize(&local).subscriptions.len(), 2);
        assert!(
            forward
                .materialize(&local)
                .subscriptions
                .iter()
                .any(|s| s.notes == "Updated elsewhere")
        );
    }
    #[test]
    fn offline_delete_does_not_resurrect_when_old_snapshots_are_read() {
        let device = Uuid::new_v4();
        let original = db();
        let mut replica = Replica::default();
        replica
            .observe_local(&Database::default(), &original, device)
            .unwrap();
        let stale = replica.clone();
        replica
            .observe_local(&original, &Database::default(), device)
            .unwrap();
        replica.merge(&stale).unwrap();
        assert!(replica.materialize(&original).subscriptions.is_empty());
    }
    #[test]
    fn concurrent_edit_delete_requires_resolution_and_preserves_both_branches() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let original = db();
        let mut left = Replica::default();
        left.observe_local(&Database::default(), &original, a)
            .unwrap();
        let mut right = left.clone();
        left.observe_local(&original, &Database::default(), a)
            .unwrap();
        let mut edited = original.clone();
        edited.subscriptions[0].notes = "Remote edit".into();
        right.observe_local(&original, &edited, b).unwrap();
        left.merge(&right).unwrap();
        assert_eq!(left.conflicts().len(), 1);
        assert!(
            left.materialize(&Database::default())
                .subscriptions
                .is_empty()
        );
        let conflict = left.conflicts().remove(0);
        let remote = conflict.choices.iter().position(Option::is_some).unwrap();
        left.resolve(&conflict, Resolution::Choose(remote), a)
            .unwrap();
        left.merge(&right).unwrap();
        assert!(left.conflicts().is_empty());
        assert_eq!(
            left.materialize(&Database::default()).subscriptions[0].notes,
            "Remote edit"
        );
    }
    #[test]
    fn editing_a_conflicted_record_does_not_discard_the_other_version() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let original = db();
        let mut left = Replica::default();
        left.observe_local(&Database::default(), &original, a)
            .unwrap();
        let mut right = left.clone();
        let mut local = original.clone();
        local.subscriptions[0].notes = "Local".into();
        left.observe_local(&original, &local, a).unwrap();
        let mut remote = original.clone();
        remote.subscriptions[0].notes = "Remote".into();
        right.observe_local(&original, &remote, b).unwrap();
        left.merge(&right).unwrap();
        let mut revised = local.clone();
        revised.subscriptions[0].notes = "Local revised".into();
        left.observe_local(&local, &revised, a).unwrap();
        assert_eq!(left.conflicts().len(), 1);
        assert!(
            left.conflicts()[0]
                .choices
                .iter()
                .any(|s| s.as_ref().is_some_and(|s| s.notes == "Remote"))
        );
        let conflict = left.conflicts().remove(0);
        left.resolve(&conflict, Resolution::KeepAll, a).unwrap();
        assert_eq!(left.materialize(&revised).subscriptions.len(), 2);
        assert!(left.conflicts().is_empty());
    }
    #[test]
    fn equal_clock_with_different_content_is_rejected_without_partial_merge() {
        let device = Uuid::new_v4();
        let original = db();
        let mut left = Replica::default();
        left.observe_local(&Database::default(), &original, device)
            .unwrap();
        let mut corrupt = left.clone();
        corrupt.records.values_mut().next().unwrap()[0]
            .value
            .as_mut()
            .unwrap()
            .name = "Corrupt".into();
        let before = left.clone();
        assert!(left.merge(&corrupt).is_err());
        assert_eq!(left, before);
    }
}
