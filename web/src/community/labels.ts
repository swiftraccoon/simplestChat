import type { Role } from '../protocol';

/** Roles an invitation may grant. */
export const INVITE_ROLES: readonly { value: Role; label: string }[] = [
  { value: 'member', label: 'Member' },
  { value: 'moderator', label: 'Moderator' },
  { value: 'admin', label: 'Admin' },
];
/** History actions as the server names them, read as what happened to the target. */
const ACTION_LABELS: Record<string, string> = {
  message_removed: 'message removed',
  kick: 'kicked',
  ban: 'banned',
  unban: 'unbanned',
  cam_ban: 'camera banned',
  cam_unban: 'camera unbanned',
  text_mute: 'text muted',
  text_unmute: 'text unmuted',
  report_resolved: 'report resolved',
  report_dismissed: 'report dismissed',
};
export const actionLabel = (action: string): string =>
  ACTION_LABELS[action] ?? action.replace(/_/g, ' ');
