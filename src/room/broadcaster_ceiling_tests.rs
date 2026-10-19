use super::*;
use crate::ids::fixtures::rid;

#[test]
fn the_deployment_ceiling_applies_beneath_a_rooms_own_broadcaster_limit() {
    let mut room = Room::new(rid("ceiling"), None, false, None);
    assert_eq!(RoomManager::broadcaster_limit(&room), None);
    room.broadcaster_ceiling = Some(30);
    assert_eq!(RoomManager::broadcaster_limit(&room), Some(30));
    let mut settings = settings::RoomSettings::defaults(&rid("ceiling"));
    settings.max_broadcasters = Some(8);
    room.settings = Some(settings);
    assert_eq!(RoomManager::broadcaster_limit(&room), Some(8));
    room.settings.as_mut().unwrap().max_broadcasters = Some(500);
    assert_eq!(RoomManager::broadcaster_limit(&room), Some(30));
    room.broadcaster_ceiling = None;
    assert_eq!(RoomManager::broadcaster_limit(&room), Some(500));
}
