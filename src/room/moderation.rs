#![forbid(unsafe_code)]

use crate::room::roles::Role;

/// In-memory punitive state for a participant
#[derive(Debug, Clone, Default)]
pub struct PunitiveState {
    pub cam_banned: bool,
    pub text_muted: bool,
}

/// Check if a participant can produce (not cam-banned, has correct role for moderated rooms)
pub fn can_produce(state: &PunitiveState, role: Role, moderated: bool, source: &str) -> bool {
    if state.cam_banned && (source == "camera" || source == "screen") {
        return false;
    }
    if moderated && !role.can_broadcast(true) {
        return false;
    }
    true
}

/// Check if a participant can chat (not text-muted, has correct role)
pub fn can_chat(state: &PunitiveState, role: Role, moderated: bool) -> bool {
    if state.text_muted {
        return false;
    }
    role.can_chat(moderated)
}
