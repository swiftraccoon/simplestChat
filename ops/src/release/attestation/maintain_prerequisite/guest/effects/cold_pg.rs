//! Host-only continuity for witnessed read-only health execs across capture cuts.
//! Explicit bootstrap commands stay complete. Reports never construct a witness.

use super::super::host::events::{Auditor, PgReason};
use super::{Error, Result, policy};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

struct Origin {
    command: Option<String>,
    phase: u8,
    valid: bool,
}
/// Captured under the same collector lock as the baseline slice.
pub(super) struct Opening {
    pg: String,
    health: String,
    start: usize,
    end: usize,
    boundary: policy::PgBoundary,
}
impl Opening {
    pub fn capture(prefix: &[Value], end: usize, pg: &str, health: &str) -> Result<Self> {
        policy::require(
            policy::digest(pg) && !health.is_empty() && prefix.len() <= end && end <= 2048,
        )?;
        let mut origins: BTreeMap<String, Origin> = BTreeMap::new();
        for row in prefix
            .iter()
            .filter(|row| row["Type"] == "container" && row["Actor"]["ID"] == pg)
        {
            let attrs = &row["Actor"]["Attributes"];
            let Some(id) = attrs["execID"].as_str().filter(|id| policy::digest(id)) else {
                continue;
            };
            let Some(action) = row["Action"].as_str() else {
                origins
                    .entry(id.into())
                    .or_insert(Origin {
                        command: None,
                        phase: 0,
                        valid: false,
                    })
                    .valid = false;
                continue;
            };
            if let Some(command) = action.strip_prefix("exec_create: ") {
                match origins.entry(id.into()) {
                    Entry::Vacant(entry) => {
                        entry.insert(Origin {
                            command: Some(command.into()),
                            phase: 1,
                            valid: true,
                        });
                    }
                    Entry::Occupied(mut entry) => entry.get_mut().valid = false,
                }
            } else if action.starts_with("exec_start: ") || action == "exec_die" {
                let origin = origins.entry(id.into()).or_insert(Origin {
                    command: None,
                    phase: 0,
                    valid: false,
                });
                if let Some(command) = &origin.command {
                    if action == format!("exec_start: {command}") && origin.phase == 1 {
                        origin.phase = 2;
                    } else if action == "exec_die" && origin.phase == 2 && attrs["exitCode"] == "0"
                    {
                        origin.phase = 3;
                    } else {
                        origin.valid = false;
                    }
                }
            } else {
                origins
                    .entry(id.into())
                    .or_insert(Origin {
                        command: None,
                        phase: 0,
                        valid: false,
                    })
                    .valid = false;
            }
        }
        let seen = origins.keys().cloned().collect();
        let seeds = origins
            .iter()
            .filter(|(_, origin)| {
                origin.valid
                    && origin.command.as_deref() == Some(health)
                    && [1, 2].contains(&origin.phase)
            })
            .map(|(id, origin)| (id.clone(), origin.phase))
            .collect();
        let origins = origins
            .into_iter()
            .filter(|(_, origin)| origin.valid)
            .filter_map(|(id, origin)| origin.command.map(|command| (id, command)))
            .collect();
        Ok(Self {
            pg: pg.into(),
            health: health.into(),
            start: prefix.len(),
            end,
            boundary: policy::PgBoundary {
                seeds,
                origins,
                seen,
            },
        })
    }
}

/// Unique health obligations; their identities and phases never serialize.
pub(super) struct ColdPg {
    pg: String,
    health: String,
    end: usize,
    phases: BTreeMap<String, u8>,
    opening_seeded: usize,
    trailing_deferred: usize,
    gap_attempted: bool,
    final_attempted: bool,
}
impl ColdPg {
    pub fn admit(
        opening: Opening,
        rows: &[Value],
        commands: &BTreeMap<String, usize>,
        audit: &mut Auditor<'_>,
    ) -> Result<Self> {
        policy::require(opening.end.checked_sub(opening.start) == Some(audit.rows())).inspect_err(
            |_| {
                audit.note_pg(
                    PgReason::IncompleteSequence,
                    None,
                    None,
                    opening.end.checked_sub(opening.start),
                    Some(audit.rows()),
                )
            },
        )?;
        let executions = policy::pg_exec_history_epoch(
            rows,
            commands,
            &opening.health,
            audit,
            &opening.boundary,
        )?;
        let tail = executions
            .iter()
            .filter_map(|(id, (command, phases))| {
                (command == &opening.health && phases != &["create", "start", "die"])
                    .then_some(id.clone())
            })
            .collect::<BTreeSet<_>>();
        let obligations = opening
            .boundary
            .seeds
            .keys()
            .cloned()
            .chain(tail.iter().cloned())
            .collect::<BTreeSet<_>>();
        let mut phases = BTreeMap::new();
        for id in obligations {
            let (command, trace) = executions.get(&id).ok_or(Error::Refused)?;
            policy::require(command == &opening.health)?;
            let phase = match trace.as_slice() {
                ["create"] => 1,
                ["create", "start"] => 2,
                ["create", "start", "die"] => 3,
                _ => return Err(Error::Refused),
            };
            phases.insert(id, phase);
        }
        Ok(Self {
            pg: opening.pg,
            health: opening.health,
            end: opening.end,
            phases,
            opening_seeded: opening.boundary.seeds.len(),
            trailing_deferred: tail.len(),
            gap_attempted: false,
            final_attempted: false,
        })
    }
    pub fn complete(&self) -> bool {
        self.phases.values().all(|phase| *phase == 3)
    }
    pub fn evidence(&self) -> Value {
        json!({"openingHealthSeeded":self.opening_seeded,
            "trailingHealthDeferred":self.trailing_deferred,
            "coldHealthObligations":self.phases.len(),
            "coldHealthCompletions":self.phases.values().filter(|phase| **phase == 3).count()})
    }
    pub fn reconcile(
        &mut self,
        prefix: &[Value],
        pending: &BTreeMap<String, u8>,
        audit: &mut Auditor<'_>,
    ) -> Result<()> {
        policy::require(
            !self.gap_attempted
                && self.end <= prefix.len()
                && prefix.len() <= 2048
                && audit.rows() == prefix.len(),
        )?;
        self.gap_attempted = true;
        policy::require(audit.begin_cold_pg_at(self.end))?;
        self.advance(&prefix[self.end..], audit)?;
        audit.post_rows();
        policy::require(self.phases.iter().all(|(id, phase)| {
            if *phase == 3 {
                !pending.contains_key(id)
            } else {
                pending.get(id) == Some(phase)
            }
        }))
        .inspect_err(|_| {
            audit.note_pg(
                PgReason::IncompleteSequence,
                Some(&self.health),
                None,
                None,
                None,
            );
        })
    }
    pub fn finish(&mut self, rows: &[Value], audit: &mut Auditor<'_>) -> Result<()> {
        policy::require(
            self.gap_attempted
                && !self.final_attempted
                && rows.len() <= 2048
                && audit.rows() == rows.len(),
        )?;
        self.final_attempted = true;
        audit.begin_cold_pg();
        self.advance(rows, audit)?;
        audit.post_rows();
        policy::require(self.complete()).inspect_err(|_| {
            audit.note_pg(
                PgReason::IncompleteSequence,
                Some(&self.health),
                None,
                Some(self.phases.len()),
                Some(self.phases.values().filter(|phase| **phase == 3).count()),
            );
        })
    }
    fn advance(&mut self, rows: &[Value], audit: &mut Auditor<'_>) -> Result<()> {
        for row in rows {
            let attrs = &row["Actor"]["Attributes"];
            let Some(id) = attrs["execID"].as_str() else {
                continue;
            };
            let Some(phase) = self.phases.get_mut(id) else {
                continue;
            };
            audit.observe(row);
            policy::require(row["Type"] == "container" && row["Actor"]["ID"] == self.pg)
                .inspect_err(|_| audit.note_pg(PgReason::ExecId, None, None, None, None))?;
            let action = policy::string(&row["Action"]).inspect_err(|_| {
                audit.note_pg(PgReason::ActionShape, None, None, None, None);
            })?;
            if let Some(command) = action.strip_prefix("exec_create: ") {
                audit.note_pg(
                    PgReason::DuplicateCreate,
                    Some(&self.health),
                    Some(command),
                    None,
                    None,
                );
                return Err(Error::Refused);
            }
            if action == format!("exec_start: {}", self.health) {
                policy::require(*phase == 1).inspect_err(|_| {
                    audit.note_pg(PgReason::StartOrder, Some(&self.health), None, None, None);
                })?;
                *phase = 2;
            } else {
                let die = action == "exec_die";
                let ordered = die && *phase == 2;
                policy::require(ordered && attrs["exitCode"] == "0").inspect_err(|_| {
                    audit.note_pg(
                        if !die {
                            PgReason::DieAction
                        } else if !ordered {
                            PgReason::DieOrder
                        } else {
                            PgReason::NonzeroExit
                        },
                        Some(&self.health),
                        action.strip_prefix("exec_start: "),
                        None,
                        None,
                    );
                })?;
                *phase = 3;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
