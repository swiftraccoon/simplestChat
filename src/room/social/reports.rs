//! Reports and the moderation log over the socket: submitting a report, the
//! moderators' report and event pages, and resolving a report.

use super::*;

impl RoomManager {
    pub(super) async fn report_participant(
        &self,
        request: &mut SocialRequest<'_>,
        target_participant_id: &ParticipantId,
        reason: &str,
    ) -> Result<SocialResponse> {
        let reason = reason.trim();
        if reason.is_empty() || reason.len() > 1024 || reason.chars().any(char::is_control) {
            return Err(invalid("Report reason must be 1–1024 bytes"));
        }
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_name = request.actor_name.clone();
        if target_participant_id == participant_id {
            return Err(invalid("Choose another participant"));
        }
        let room = request.room();
        let target = room
            .participants
            .get(target_participant_id)
            .ok_or_else(|| not_found("Participant is no longer in this room"))?;
        let target_name = target.name.clone();
        let reporter = reporter_key(room.participants.get(participant_id).unwrap());
        if room
            .social
            .report_cooldown_active(&reporter, std::time::Instant::now())
        {
            return Err(rate_limited("Please wait before submitting another report"));
        }
        let report_id = Uuid::new_v4();
        let report = ReportEntry {
            report_id: report_id.to_string(),
            reporter_id: *participant_id,
            reporter_name: actor_name,
            target_participant_id: *target_participant_id,
            target_name,
            reason: reason.to_string(),
            status: ReportStatus::Open,
            created_at: chrono::Utc::now().to_rfc3339(),
            resolved_at: None,
            outcome: None,
        };
        if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("Report service unavailable"))?;
            request.release();
            let inserted = self
                .persist_room(room_id, request.room_lock, async {
                    let mut transaction = pool.begin().await?;
                    let inserted =
                        moderation::file_report(&mut transaction, room_id, &report, MAX_REPORTS)
                            .await?;
                    transaction.commit().await?;
                    Ok(inserted)
                })
                .await?;
            if !inserted {
                return Err(forbidden(
                    "Room report capacity reached; contact a moderator",
                ));
            }
            request.reacquire().await?;
        } else {
            if room.social.reports.len() >= MAX_REPORTS {
                if let Some(index) = room
                    .social
                    .reports
                    .iter()
                    .position(|r| r.status != ReportStatus::Open)
                {
                    room.social.reports.remove(index);
                } else {
                    return Err(forbidden(
                        "Room report capacity reached; contact a moderator",
                    ));
                }
            }
            room.social.reports.push_back(report.clone());
        }
        request
            .room()
            .social
            .note_report(reporter, std::time::Instant::now());
        // Only the submitting participant receives the report ID here.
        Ok(SocialResponse::ReportParticipant {
            report_id: report.report_id.clone(),
            status: ReportStatus::Open,
        })
    }

    pub(super) async fn list_room_reports(
        &self,
        request: &mut SocialRequest<'_>,
        offset: Option<u32>,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let offset = page_offset(offset)?;
        let room_id = request.room_id;
        let room = request.room();
        let mut reports: Vec<ReportEntry> = if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("Report service unavailable"))?;
            request.release();
            let reports = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                moderation::list_reports(pool, room_id, offset as i64, (PAGE_SIZE + 1) as i64),
            )
            .await
            .map_err(|_| timed_out("Report service timed out"))??;
            request.reacquire().await?;
            reports
        } else {
            room.social
                .reports
                .iter()
                .rev()
                .skip(offset)
                .take(PAGE_SIZE + 1)
                .map(|report| ReportEntry {
                    outcome: room.social.report_outcome(&report.report_id),
                    ..report.clone()
                })
                .collect()
        };
        let has_more = reports.len() > PAGE_SIZE;
        reports.truncate(PAGE_SIZE);
        Ok(SocialResponse::ListRoomReports { reports, has_more })
    }

    pub(super) async fn resolve_room_report(
        &self,
        request: &mut SocialRequest<'_>,
        report_id: &str,
        status: moderation::ReportResolution,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let id: Uuid = report_id.parse().map_err(|_| invalid("Invalid report"))?;
        let action = status.action();
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_name = request.actor_name.clone();
        let room = request.room();
        if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("Report service unavailable"))?;
            request.release();
            let actor_id = *participant_id;
            let resolved = self
                .persist_room(room_id, request.room_lock, async {
                    let mut transaction = pool.begin().await?;
                    let resolved = moderation::resolve_report(
                        &mut transaction,
                        room_id,
                        id,
                        status,
                        Actor {
                            id: &actor_id,
                            name: &actor_name,
                        },
                    )
                    .await?;
                    transaction.commit().await?;
                    Ok(resolved)
                })
                .await?;
            request.reacquire().await?;
            if !resolved {
                return Err(not_found("Open report not found"));
            }
        } else {
            let report = room
                .social
                .reports
                .iter_mut()
                .find(|r| r.report_id == *report_id && r.status == ReportStatus::Open)
                .ok_or_else(|| not_found("Open report not found"))?;
            report.status = status.status();
            report.resolved_at = Some(chrono::Utc::now().to_rfc3339());
            let target_id = report.target_participant_id;
            let target_name = report.target_name.clone();
            let target_authenticated = room
                .participants
                .get(&target_id)
                .is_some_and(|participant| participant.authenticated);
            room.social.record_moderation_event(ModerationEvent::new(
                action,
                Actor {
                    id: participant_id,
                    name: &actor_name,
                },
                Target {
                    id: target_id.into(),
                    name: &target_name,
                    authenticated: target_authenticated,
                    ip: None,
                },
                None,
                None,
                Some(id),
            ));
        }
        Ok(SocialResponse::ResolveRoomReport {
            report_id: report_id.to_string(),
            status,
        })
    }

    pub(super) async fn list_moderation_events(
        &self,
        request: &mut SocialRequest<'_>,
        offset: Option<u32>,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let offset = page_offset(offset)?;
        let owner = request.actor_role >= roles::Role::Owner;
        let room_id = request.room_id;
        let room = request.room();
        let mut events: Vec<ModerationEvent> = if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("History service unavailable"))?;
            request.release();
            let rows = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                moderation::list_events(pool, room_id, offset as i64, (PAGE_SIZE + 1) as i64),
            )
            .await
            .map_err(|_| timed_out("History service timed out"))??;
            request.reacquire().await?;
            rows
        } else {
            room.social
                .moderation_events
                .iter()
                .rev()
                .skip(offset)
                .take(PAGE_SIZE + 1)
                .cloned()
                .collect()
        };
        let has_more = events.len() > PAGE_SIZE;
        events.truncate(PAGE_SIZE);
        // Only the owner reads a target's address; moderators never do.
        let events: Vec<ModerationEventView> = events
            .iter()
            .map(|event| ModerationEventView::new(event.clone(), owner))
            .collect();
        Ok(SocialResponse::ListModerationEvents { events, has_more })
    }
}
