import type { ChatEntry, ChatStyle } from '../protocol';
import type { RoomClient } from '../room';
import { renderAttachments } from '../attachments';
import { asyncButton, button, busy, el, modal } from '../ui';
import { appendLinkedText, appendEditedLabel, editMessageDialog } from '../chat-message-ui';
import { chatColor } from '../avatar-colors';
import { CHAT_REACTIONS } from '../chat-reactions';
import { openRoomHistory, notifyHistoryEdit } from '../chat-history';
import type { ChatStore, ChatItem } from './store';
import type { SocialChatOptions } from './context';
import type { Preferences } from './preferences';
import { AUTOMATIC_LOOK, formatChatTime } from './format';

type ConversationButton = { node: HTMLButtonElement; name: HTMLElement; unread: HTMLElement };

type MessageRow = { node: HTMLElement; fingerprint: string; dispose: () => void };

interface RenderContext {
  readonly options: Pick<
    SocialChatOptions,
    'getRoom' | 'getViewerKey' | 'getToken' | 'notify' | 'bindParticipantName'
  >;
  readonly store: ChatStore;
  readonly preferences: Preferences;
  readonly activeRoom: RoomClient | null;
  readonly activation: number;
  readonly viewerKey: string;
  readonly pins: readonly ChatEntry[];
  readonly replyingTo: ChatEntry | null;
  readonly seenAtBottom: boolean;
  readonly accountPeers: ReadonlySet<string>;
  contextCurrent: (activation: number, room: RoomClient, viewer: string) => boolean;
  isIgnored: (id: string) => boolean;
  pinMessage: (message: ChatEntry, pinned: boolean) => void;
  startReply: (message: ChatEntry) => void;
  cancelReply: () => void;
  render: (forceScroll?: boolean) => void;
  retry: (message: ChatItem) => void;
  restoreDraft: (message: ChatItem) => void;
  switchConversation: (id: string, name?: string) => void;
}

/** Owns retained message nodes and attachment grants, scoped to exact chat activation. */
export class ChatRenderer {
  private readonly context: RenderContext;

  readonly rows = new Map<string, MessageRow>();
  readonly conversations = el('nav', undefined, 'conversation-list');
  readonly conversationButtons = new Map<string, ConversationButton>();
  visibleConversation = '';

  constructor(context: RenderContext) {
    this.context = context;
  }

  reset(): void {
    for (const row of this.rows.values()) {
      row.dispose();
      row.node.remove();
    }
    this.rows.clear();
  }

  rowKey(message: ChatEntry): string {
    // Match the store's correlation identity: the server replaces the provisional
    // message ID on acknowledgement without replacing this visible message.
    return JSON.stringify([
      message.participantId,
      message.clientMessageId,
      message.recipientId ?? 'public',
    ]);
  }

  messageRow(message: ChatItem, nickname: string | undefined): HTMLElement {
    const { messageId, participantId, participantName, content, sentAt, status, error } = message;
    const local = participantId === this.context.store.localId;
    const mentioned = Boolean(
      nickname && !local && content.toLowerCase().includes(`@${nickname.toLowerCase()}`),
    );
    const look = participantId ? this.lookOf(message, local) : null;
    const color = look ? chatColor(participantName, look.color) : '';
    // ChatStore updates objects in place. Compare immutable display/action values,
    // never object identity; grouping depends on adjacent visible rows instead.
    const fingerprint = JSON.stringify([
      messageId,
      participantId,
      participantName,
      content,
      sentAt,
      status,
      error,
      local,
      mentioned,
      !!message.retry,
      this.context.preferences.timestamps,
      look?.style,
      color,
      message.replyTo,
      message.reactions,
      message.removedAt,
      message.editedAt,
      message.revision,
      message.attachments,
      this.canRemove(message),
      this.context.pins.some((pin) => pin.messageId === messageId),
    ]);
    const key = this.rowKey(message);
    const previous = this.rows.get(key);
    if (previous?.fingerprint === fingerprint) return previous.node;
    previous?.dispose();
    const node = previous?.node ?? el('div');
    node.className = `chat-msg${look ? ` look-${look.style}` : ' system'}${mentioned ? ' mentioned' : ''}`;
    if (look) node.style.setProperty('--sender-color', color);
    node.dataset['messageId'] = messageId;
    node.replaceChildren();
    if (participantId) {
      const sender = el('button', participantName, 'sender chat-sender-button');
      sender.type = 'button';
      this.context.options.bindParticipantName(sender, participantId, participantName);
      node.append(sender);
    }
    if (message.replyTo) {
      const reply = message.replyTo;
      const quote = button(
        this.context.isIgnored(reply.participantId)
          ? 'Reply to a message you have hidden'
          : `${reply.participantName}: ${reply.excerpt}`,
        () => this.showMessage(reply.messageId),
        'msg-reply',
      );
      quote.title = 'Show the message this answers';
      node.append(quote);
    }
    const text = el('div', undefined, 'msg-text');
    if (message.removedAt) {
      text.textContent = 'Message removed';
      text.classList.add('muted');
    } else appendLinkedText(text, content, mentioned ? `@${nickname}` : undefined);
    node.append(text);
    let dispose = (): void => {};
    const room = this.context.activeRoom;
    if (!message.removedAt && message.attachments?.length && room) {
      const activation = this.context.activation;
      const viewer = this.context.viewerKey;
      const current = () =>
        this.context.contextCurrent(activation, room, viewer) &&
        this.rows.get(key)?.node === node &&
        !message.removedAt;
      dispose = renderAttachments(node, message.attachments, {
        current,
        authorization: async (attachmentId) => {
          if (!current()) throw new Error('This conversation changed');
          const grant = await room.requestSocial('getAttachmentAccess', { attachmentId });
          return `Attachment ${grant.token}`;
        },
      });
    }
    this.appendReactions(node, message);
    if (participantId && status === 'sent' && !message.removedAt) this.appendActions(node, message);
    const meta = el('time', undefined, 'msg-time');
    const date = new Date(sentAt);
    meta.textContent = formatChatTime(date, this.context.preferences.timestamps);
    meta.setAttribute('datetime', sentAt);
    meta.title = date.toLocaleString();
    node.append(meta);
    appendEditedLabel(node, message);
    if (participantId) {
      // Delivery state stays visible even when the viewer chooses hover-only timestamps.
      if (status === 'pending' || status === 'failed' || status === 'unknown') {
        const state = el(
          'div',
          status === 'pending' ? 'Sending…' : (error ?? 'Delivery failed'),
          'msg-status',
        );
        if (status !== 'pending') {
          state.classList.add('delivery-error');
          if (status === 'unknown' && message.retry)
            state.append(
              button('Retry same message', () => this.context.retry(message), 'auth-link-btn'),
            );
          state.append(
            button(
              status === 'unknown' ? 'Copy to draft' : 'Edit & resend',
              () => this.context.restoreDraft(message),
              'auth-link-btn',
            ),
          );
        }
        node.append(state);
      }
    }
    this.rows.set(key, { node, fingerprint, dispose });
    return node;
  }

  /** Reaction chips; each toggles the viewer's own. Ignored people are left out. */
  appendReactions(node: HTMLElement, message: ChatItem): void {
    const reactions = (message.reactions ?? [])
      .map((reaction) => ({
        emoji: reaction.emoji,
        people: reaction.participantIds.filter((id) => !this.context.isIgnored(id)),
      }))
      .filter((reaction) => reaction.people.length);
    if (!reactions.length) return;
    const bar = el('div', undefined, 'msg-reactions');
    for (const reaction of reactions) {
      const chip = button(
        `${reaction.emoji} ${reaction.people.length}`,
        () => this.react(message, reaction.emoji),
        'reaction-chip',
      );
      chip.setAttribute(
        'aria-pressed',
        String(reaction.people.includes(this.context.store.localId ?? '')),
      );
      chip.title = reaction.people.map((id) => this.personName(id)).join(', ');
      bar.append(chip);
    }
    node.append(bar);
  }

  /** React and Reply, floating above a message on hover, focus or a tap. */
  appendActions(node: HTMLElement, message: ChatItem): void {
    const actions = el('div', undefined, 'msg-actions');
    const picker = el('div', undefined, 'reaction-picker');
    picker.hidden = true;
    for (const emoji of CHAT_REACTIONS)
      picker.append(
        button(
          emoji,
          () => {
            picker.hidden = true;
            this.react(message, emoji);
          },
          'msg-action',
        ),
      );
    const react = button(
      '☺',
      () => {
        picker.hidden = !picker.hidden;
      },
      'msg-action',
    );
    const reply = button('↩', () => this.context.startReply(message), 'msg-action');
    for (const [control, label] of [
      [react, 'Add a reaction'],
      [reply, 'Reply'],
    ] as const) {
      control.setAttribute('aria-label', label);
      control.title = label;
    }
    actions.append(react, reply, picker);
    if (message.participantId === this.context.store.localId) {
      actions.append(button('Edit', () => this.editMessage(message), 'msg-action'));
    }
    if (this.canRemove(message)) {
      const pinned = this.context.pins.some((pin) => pin.messageId === message.messageId);
      actions.append(
        button(
          pinned ? 'Unpin' : 'Pin',
          () => this.context.pinMessage(message, !pinned),
          'msg-action',
        ),
      );
    }
    if (this.canRemove(message)) {
      const remove = button('Remove', () => this.confirmRemoval(message), 'msg-action');
      remove.setAttribute('aria-label', 'Remove message');
      remove.title = 'Remove message for everyone';
      actions.append(remove);
    }
    node.append(actions);
    // Touch screens have no hover: a tap on the message shows its actions.
    node.onclick = (event) => {
      if (!(event.target as Element).closest('button, a')) node.classList.toggle('show-actions');
    };
  }

  editMessage(message: ChatItem): void {
    const room = this.context.options.getRoom();
    if (!room || message.participantId !== this.context.store.localId || message.removedAt) return;
    const activation = this.context.activation;
    const viewer = this.context.viewerKey;
    editMessageDialog(message, {
      current: () => this.context.contextCurrent(activation, room, viewer),
      save: async (content, expectedRevision) =>
        (
          await room.requestSocial('editChatMessage', {
            messageId: message.messageId,
            content,
            expectedRevision,
          })
        ).message,
      changed: notifyHistoryEdit,
    });
  }

  canRemove(message: ChatItem): boolean {
    return (
      !message.recipientId &&
      !message.removedAt &&
      ['moderator', 'admin', 'owner'].includes(this.context.options.getRoom()?.role ?? '')
    );
  }

  confirmRemoval(message: ChatItem): void {
    const room = this.context.options.getRoom();
    if (!room || !this.canRemove(message)) return;
    const activation = this.context.activation;
    const viewer = this.context.viewerKey;
    const current = (): boolean => this.context.contextCurrent(activation, room, viewer);
    const dialog = modal('Remove message?');
    dialog.body.append(
      el(
        'p',
        'This removes the public message and its quoted text for everyone. A “Message removed” marker will remain.',
      ),
    );
    const remove = asyncButton(
      'Remove message',
      async () => {
        await busy(remove, dialog.error, async () => {
          if (!current()) throw new Error('The room session changed.');
          const result = await room.requestSocial('removeChatMessage', {
            messageId: message.messageId,
          });
          if (current()) {
            this.context.store.removeMessage(result.messageId, result.removedAt);
            if (this.context.replyingTo?.messageId === result.messageId) this.context.cancelReply();
            this.context.render();
          }
          dialog.close();
        });
      },
      (error) =>
        this.context.options.notify(
          error instanceof Error ? error.message : 'Could not remove message',
        ),
    );
    dialog.body.append(button('Cancel', dialog.close), remove);
  }

  react(message: ChatItem, emoji: string): void {
    const room = this.context.options.getRoom();
    if (!room) return;
    const activation = this.context.activation;
    const viewer = this.context.viewerKey;
    room
      .requestSocial('reactToMessage', { messageId: message.messageId, emoji })
      .then((result) => {
        if (!this.context.contextCurrent(activation, room, viewer)) return;
        if (this.context.store.setReactions(result.messageId, result.reactions))
          this.context.render();
      })
      .catch((error: unknown) => {
        if (this.context.contextCurrent(activation, room, viewer))
          this.context.options.notify(
            error instanceof Error ? error.message : 'Could not add the reaction',
          );
      });
  }

  personName(id: string): string {
    const room = this.context.options.getRoom();
    return (
      (id === this.context.store.localId
        ? room?.nickname
        : room?.getParticipants().get(id)?.name) ?? 'someone who left'
    );
  }

  /** Brings the message a reply quotes into view, if it is still in this chat. */
  showMessage(messageId: string): void {
    const row = [...this.rows.values()].find(
      (entry) => entry.node.dataset['messageId'] === messageId,
    );
    if (!row) {
      const room = this.context.options.getRoom();
      if (room?.roomSettings?.historyRetentionDays && this.context.store.active === 'public') {
        const activation = this.context.activation;
        const viewer = this.context.viewerKey;
        openRoomHistory(
          room,
          () => this.context.contextCurrent(activation, room, viewer),
          !!this.context.options.getToken?.(),
          messageId,
          (message) => {
            this.context.switchConversation('public');
            this.context.startReply(message);
          },
        );
      } else this.context.options.notify('That message is no longer in this chat');
      return;
    }
    row.node.scrollIntoView?.({ block: 'center', behavior: 'smooth' });
    row.node.classList.toggle('flash', true);
    setTimeout(() => row.node.classList.toggle('flash', false), 1200);
  }

  /** The look a message was sent with; entries without one wear the sender's current look. */
  lookOf(message: ChatEntry, local: boolean): ChatStyle {
    const room = this.context.options.getRoom();
    const current = local
      ? room?.chatStyle
      : room?.getParticipants().get(message.participantId)?.chatStyle;
    return message.chatStyle ?? current ?? AUTOMATIC_LOOK;
  }

  updateBadges(): void {
    const total = [...this.context.store.unread.values()].reduce((sum, count) => sum + count, 0);
    document.title = total ? `(${Math.min(total, 999)}) simplestChat` : 'simplestChat';
    const badge = document.getElementById('unread-badge')!;
    const activeUnread = this.context.store.unread.get(this.context.store.active) ?? 0;
    badge.textContent = String(activeUnread);
    badge.hidden = !activeUnread;
    document.getElementById('scroll-bottom-btn')!.hidden = this.context.seenAtBottom;
    const tab = document.querySelector<HTMLButtonElement>('[data-tab="chat"]');
    if (tab) {
      let count = tab.querySelector('.conversation-unread');
      if (!count) {
        count = el('span', '', 'conversation-unread');
        tab.append(count);
      }
      count.textContent = total ? ` ${total}` : '';
    }
    this.renderConversations();
  }

  /** Keep each button in place while labels and unread counts change around keyboard focus. */
  renderConversations(): void {
    const names = new Map([['public', 'Public chat'], ...this.context.store.names]);
    for (const [id, entry] of this.conversationButtons) {
      if (names.has(id)) continue;
      const focused = document.activeElement === entry.node;
      entry.node.remove();
      this.conversationButtons.delete(id);
      if (focused) this.conversationButtons.get('public')?.node.focus();
    }
    for (const [id, name] of names) {
      let entry = this.conversationButtons.get(id);
      if (!entry) {
        const node = button('', () => this.context.switchConversation(id), 'conversation-tab');
        node.dataset['conversationId'] = id;
        entry = {
          node,
          name: el('span', '', 'conversation-name'),
          unread: el('span', '', 'conversation-unread'),
        };
        entry.unread.setAttribute('aria-hidden', 'true');
        node.append(entry.name, entry.unread);
        this.conversationButtons.set(id, entry);
        this.conversations.append(node);
      }
      const privateChat = id !== 'public';
      const offline =
        privateChat &&
        !this.context.accountPeers.has(id) &&
        !this.context.options.getRoom()?.getParticipants().has(id);
      const label = `${name}${offline ? ' · offline' : ''}`;
      if (entry.name.textContent !== label) entry.name.textContent = label;
      const unread = this.context.store.unread.get(id) ?? 0;
      entry.unread.textContent = unread ? String(unread) : '';
      entry.unread.hidden = !unread;
      entry.node.title = label;
      entry.node.setAttribute('aria-label', `${label}${unread ? `. ${unread} unread` : ''}`);
      entry.node.setAttribute('aria-pressed', String(id === this.context.store.active));
    }
    if (this.visibleConversation !== this.context.store.active) {
      this.visibleConversation = this.context.store.active;
      this.conversationButtons
        .get(this.context.store.active)
        ?.node.scrollIntoView?.({ block: 'nearest', inline: 'nearest' });
    }
  }
}
