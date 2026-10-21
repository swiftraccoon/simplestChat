import { createScope } from '../disposable';
import type { ChatAttachment, ChatEntry } from '../protocol';
import type { RoomClient } from '../room';
import type { TelemetryHandler, TelemetryOutcome } from '../telemetry-types';
import { AttachmentComposer } from '../attachments';
import { button, el } from '../ui';
import { ConversationInputs, type ChatStore, type ChatItem } from './store';
import type { SocialChatOptions } from './context';
import { AUTOMATIC_LOOK, excerpt } from './format';
import { ChatMentions } from './mentions';

interface ComposerContext {
  readonly elements: {
    readonly input: HTMLTextAreaElement;
    readonly inputRow: HTMLElement;
    readonly sendButton: HTMLButtonElement;
  };
  readonly options: Pick<
    SocialChatOptions,
    'getRoom' | 'getViewerKey' | 'getToken' | 'notify' | 'telemetry'
  >;
  readonly store: ChatStore;
  readonly activeRoom: RoomClient | null;
  readonly activation: number;
  readonly viewerKey: string;
  readonly membershipVersion: number;
  readonly seenAtBottom: boolean;
  readonly messages: HTMLElement;
  contextCurrent: (activation: number, room: RoomClient, viewer: string) => boolean;
  render: (forceScroll?: boolean) => void;
  switchConversation: (id: string, name?: string) => void;
  clearDivider: () => void;
}

/** Owns drafts, completion, attachments and pending send attempts for one chat activation. */
export class ChatComposer {
  private readonly context: ComposerContext;

  readonly attachments: AttachmentComposer;
  readonly mentions: ChatMentions;
  private readonly events = createScope();
  private readonly observer: ResizeObserver;
  readonly composition = new ConversationInputs();
  readonly touchPointer = window.matchMedia('(any-pointer: coarse)');
  touchComposer = false;
  /** Who is composing in the visible conversation, until each notice expires. */
  readonly typingLine = el('div', '', 'typing-line');
  typing = new Map<string, { until: number; target: string | null }>();
  typingTimer: ReturnType<typeof setTimeout> | null = null;
  lastTypingSent = 0;
  readonly emojiPanel = el('div', undefined, 'emoji-panel');
  readonly pendingStarted = new Map<string, number>();
  readonly pending = new Map<string, ReturnType<typeof setTimeout>>();
  readonly retrying = new Set<string>();
  chatSessionId: string | null = null;
  chatSequence = 0;
  // The message the next send answers, shown above the message box until sent or cancelled.
  replyingTo: ChatEntry | null = null;
  readonly replyBar = el('div', undefined, 'reply-bar');

  constructor(context: ComposerContext) {
    this.context = context;

    const { input, inputRow, sendButton } = context.elements;
    const signal = this.events.signal;
    this.mentions = new ChatMentions({
      input,
      getRoom: context.options.getRoom,
      insertText: (text, start, end) => this.insertText(text, start, end),
    });
    this.typingLine.hidden = true;
    this.typingLine.setAttribute('aria-live', 'polite');
    inputRow.before(this.typingLine);
    const emojiButton = button(
      '☺',
      () => {
        this.emojiPanel.hidden = !this.emojiPanel.hidden;
      },
      'emoji-toggle',
    );
    emojiButton.title = 'Choose emoji';
    emojiButton.setAttribute('aria-label', 'Choose emoji');
    for (const emoji of ['😀', '😂', '😊', '❤️', '👍', '👋', '🎉', '🔥', '🤔', '😢', '🙌', '💯']) {
      this.emojiPanel.append(
        button(
          emoji,
          () => {
            this.insertText(
              emoji,
              input.selectionStart ?? input.value.length,
              input.selectionEnd ?? input.value.length,
            );
            this.emojiPanel.hidden = true;
            input.focus();
          },
          'emoji-choice',
        ),
      );
    }
    this.emojiPanel.hidden = true;
    inputRow.prepend(emojiButton);
    inputRow.before(this.emojiPanel);
    input.maxLength = 2000;
    inputRow.before(this.mentions.mentionList);
    this.replyBar.hidden = true;
    inputRow.before(this.replyBar);
    input.addEventListener('input', () => this.mentions.suggestMentions(), { signal });
    input.addEventListener('input', () => this.noteTyping(), { signal });
    input.addEventListener('blur', () => this.mentions.closeMentions(), { signal });
    input.addEventListener('keydown', (event) => this.onKey(event), { signal });
    input.addEventListener(
      'input',
      () => this.composition.save(context.store.active, input.value),
      { signal },
    );
    input.addEventListener('input', () => this.resizeInput(), { signal });
    this.touchPointer.addEventListener('change', () => this.updateComposerMode(), { signal });
    this.updateComposerMode();
    let width = 0;
    this.observer = new ResizeObserver(([entry]) => {
      if (this.events.signal.aborted) return;
      const next = entry?.contentRect.width ?? 0;
      if (next === width) return;
      width = next;
      this.resizeInput();
    });
    this.observer.observe(input);
    this.events.onDispose(() => this.observer.disconnect());
    sendButton.addEventListener('click', () => this.send(), { signal });
    this.attachments = new AttachmentComposer({
      host: inputRow,
      input,
      getToken: () => context.options.getToken?.() ?? null,
      current: () =>
        context.activeRoom !== null &&
        context.contextCurrent(context.activation, context.activeRoom, context.viewerKey),
      notify: context.options.notify,
    });
  }

  reset(): void {
    this.attachments.reset();
    for (const timer of this.pending.values()) clearTimeout(timer);
    for (const id of this.pendingStarted.keys()) this.finishSend(id, 'superseded');
    this.pending.clear();
    this.retrying.clear();
    this.chatSessionId = null;
    this.chatSequence = 0;
    this.cancelReply();
    this.composition.reset();
    this.typing.clear();
    this.lastTypingSent = 0;
    this.renderTyping();
    this.mentions.closeMentions();
    this.emojiPanel.hidden = true;
    this.context.elements.input.value = '';
  }

  dispose(): void {
    this.events.dispose();
    this.reset();
    this.attachments.dispose();
    this.mentions.dispose();
  }

  /** Composing in an open conversation tells its recipients, at most every 2.5 s. */
  noteTyping(): void {
    if (this.events.signal.aborted) return;
    const room = this.context.options.getRoom();
    if (
      !room?.localParticipantId ||
      !room.connected ||
      !room.canChat ||
      this.context.elements.input.disabled
    )
      return;
    if (!this.context.elements.input.value.trim()) return;
    const now = Date.now();
    if (now - this.lastTypingSent < 2500) return;
    this.lastTypingSent = now;
    room.sendTyping(this.context.store.active === 'public' ? undefined : this.context.store.active);
  }

  noteRemoteTyping(id: string, target: string | null): void {
    if (id === this.context.store.localId) return;
    this.typing.set(id, { until: Date.now() + 4000, target });
    this.renderTyping();
  }

  /** "Alice is typing…" for the visible conversation only; notices expire on their own. */
  renderTyping(): void {
    const now = Date.now();
    for (const [id, entry] of this.typing) if (entry.until <= now) this.typing.delete(id);
    const room = this.context.options.getRoom();
    const names: string[] = [];
    for (const [id, entry] of this.typing) {
      const visible =
        this.context.store.active === 'public'
          ? entry.target === null
          : id === this.context.store.active && entry.target === this.context.store.localId;
      if (!visible) continue;
      const name = room?.getParticipants().get(id)?.name ?? this.context.store.names.get(id);
      if (name) names.push(name);
    }
    this.typingLine.hidden = names.length === 0;
    this.typingLine.textContent =
      names.length === 0
        ? ''
        : names.length === 1
          ? `${names[0]} is typing…`
          : names.length === 2
            ? `${names[0]} and ${names[1]} are typing…`
            : 'Several people are typing…';
    if (this.typingTimer !== null) clearTimeout(this.typingTimer);
    this.typingTimer = null;
    if (this.typing.size) {
      const next = Math.min(...[...this.typing.values()].map((entry) => entry.until));
      this.typingTimer = setTimeout(() => this.renderTyping(), Math.max(0, next - now));
    }
  }

  send(): void {
    if (this.events.signal.aborted) return;
    const room = this.context.options.getRoom();
    const content = this.context.elements.input.value.trim();
    if (
      !room?.localParticipantId ||
      !room.connected ||
      (!content && !this.attachments.hasFiles) ||
      this.context.elements.input.disabled
    )
      return;
    let attachments: ChatAttachment[];
    try {
      attachments = this.attachments.ready();
    } catch (error) {
      this.context.options.notify(
        error instanceof Error ? error.message : 'Attachments are not ready',
      );
      return;
    }
    if (new TextEncoder().encode(content).length > 4096) {
      this.context.options.notify('Message is too long (maximum 4096 bytes)');
      return;
    }
    if (this.pending.size >= 100) {
      this.context.options.notify('Please wait for pending messages');
      return;
    }
    const id = crypto.randomUUID();
    if (this.chatSequence >= Number.MAX_SAFE_INTEGER) return;
    const sequence = ++this.chatSequence;
    const recipientId =
      this.context.store.active === 'public' ? undefined : this.context.store.active;
    const recipientName =
      recipientId === undefined ? undefined : this.context.store.names.get(recipientId);
    // Sending shows the conversation is caught up; the divider has served its purpose.
    this.context.clearDivider();
    const answering =
      this.replyingTo &&
      this.context.store.conversation(this.replyingTo) === this.context.store.active
        ? this.replyingTo
        : null;
    const retained = this.context.store.pending(
      {
        messageId: `pending:${id}`,
        ...(answering && {
          replyTo: {
            messageId: answering.messageId,
            participantId: answering.participantId,
            participantName: answering.participantName,
            excerpt: excerpt(answering.content),
          },
        }),
        clientMessageId: id,
        participantId: room.localParticipantId,
        participantName: room.nickname,
        ...(recipientId !== undefined && { recipientId }),
        ...(recipientName !== undefined && { recipientName }),
        content,
        ...(attachments.length && { attachments }),
        sentAt: new Date().toISOString(),
        revision: 0,
        // The look you have now; the acknowledgement carries the look the server stored.
        chatStyle: room.chatStyle ?? AUTOMATIC_LOOK,
      },
      this.chatSessionId === null
        ? undefined
        : {
            sequence,
            chatSessionId: this.chatSessionId,
            expiresAt: Date.now() + 300_000,
          },
    );
    if (!retained) {
      this.context.options.notify('This message could not be added to the conversation');
      return;
    }
    this.startPending(id);
    try {
      const attachmentIds = attachments.length
        ? attachments.map((attachment) => attachment.id)
        : undefined;
      if (recipientId)
        room.sendPrivate(
          recipientId,
          content,
          id,
          sequence,
          answering?.messageId,
          ...(attachmentIds ? ([attachmentIds] as const) : ([] as const)),
        );
      else
        room.sendChat(
          content,
          id,
          sequence,
          answering?.messageId,
          ...(attachmentIds ? ([attachmentIds] as const) : ([] as const)),
        );
      this.attachments.consume();
      this.composition.sent(this.context.store.active, content);
      this.context.elements.input.value = '';
      this.cancelReply();
    } catch (error) {
      this.finishSend(id, 'error');
      this.clearPending(id);
      this.context.store.fail(id, error instanceof Error ? error.message : 'Send failed');
    }
    this.context.render(true);
  }

  startPending(id: string): void {
    this.pendingStarted.set(id, performance.now());
    this.recordTelemetry({ name: 'chat_send', outcome: 'started' });
    this.pending.set(
      id,
      setTimeout(() => {
        this.finishSend(id, 'timeout');
        this.pending.delete(id);
        this.retrying.delete(id);
        this.context.store.fail(
          id,
          this.context.store.messages.some(
            (message) => message.clientMessageId === id && message.retry,
          )
            ? 'Delivery not confirmed. Retry checks whether this message was accepted; available for up to five minutes.'
            : 'Delivery not confirmed. This send has no retry session; reconnecting may recover its acknowledgement.',
          true,
        );
        this.context.render();
      }, 12_000),
    );
  }

  retireDeliveryAttempts(): void {
    for (const id of this.pending.keys()) this.clearPending(id);
    this.context.store.retireAttempts();
    this.chatSessionId = null;
    this.chatSequence = 0;
  }

  retry(message: ChatItem): void {
    const room = this.context.options.getRoom();
    if (message.status !== 'unknown' || !this.context.store.messages.includes(message)) return;
    const attempt = message.retry;
    if (
      !attempt ||
      Date.now() >= attempt.expiresAt ||
      room !== this.context.activeRoom ||
      room?.membershipVersion !== this.context.membershipVersion ||
      this.context.viewerKey !== this.context.options.getViewerKey() ||
      attempt.chatSessionId !== this.chatSessionId
    ) {
      delete message.retry;
      this.context.store.fail(
        message.clientMessageId,
        'Delivery not confirmed. The retry window or room session ended.',
        true,
      );
      this.context.render();
      return;
    }
    if (!room.connected || this.pending.size >= 100) {
      this.context.options.notify('Wait for the room connection before retrying');
      return;
    }
    message.status = 'pending';
    delete message.error;
    this.startPending(message.clientMessageId);
    this.retrying.add(message.clientMessageId);
    try {
      room.retryChat({
        clientMessageId: message.clientMessageId,
        sequence: attempt.sequence,
        chatSessionId: attempt.chatSessionId,
        content: message.content,
        ...(message.recipientId !== undefined && { targetParticipantId: message.recipientId }),
        ...(message.replyTo && { replyTo: message.replyTo.messageId }),
        ...(message.attachments?.length && {
          attachmentIds: message.attachments.map((attachment) => attachment.id),
        }),
      });
    } catch {
      this.finishSend(message.clientMessageId, 'unknown');
      this.clearPending(message.clientMessageId);
      this.context.store.fail(
        message.clientMessageId,
        'Delivery not confirmed. Wait for the connection and retry.',
        true,
      );
    }
    this.context.render();
  }

  restoreDraft(message: ChatItem): void {
    const room = this.context.options.getRoom();
    if (
      room !== this.context.activeRoom ||
      room?.membershipVersion !== this.context.membershipVersion ||
      this.context.viewerKey !== this.context.options.getViewerKey() ||
      !this.context.store.messages.includes(message)
    )
      return;
    const conversation = this.context.store.conversation(message);
    const draft =
      conversation === this.context.store.active
        ? this.context.elements.input.value
        : this.composition.draft(conversation);
    if (draft.trim() && draft !== message.content) {
      this.context.options.notify('Keep or clear the existing draft before copying this message');
      return;
    }
    this.context.switchConversation(conversation, message.recipientName);
    this.context.elements.input.value = message.content;
    this.composition.save(conversation, message.content);
    this.resizeInput();
    if (message.attachments?.length)
      this.context.options.notify(
        'Attachments stay with the original message. Use Retry same message when available, or attach the files again for a new message.',
      );
    this.context.elements.input.focus();
    if (message.status === 'unknown')
      this.context.options.notify(
        'Delivery was not confirmed. Sending a new message may duplicate the original.',
      );
  }

  recordTelemetry(event: Parameters<TelemetryHandler>[0]): void {
    try {
      this.context.options.telemetry?.(event);
    } catch {
      /* Delivery state must not depend on its observer. */
    }
  }

  finishSend(id: string, outcome: TelemetryOutcome): void {
    const started = this.pendingStarted.get(id);
    if (started === undefined) return;
    this.pendingStarted.delete(id);
    this.recordTelemetry({
      name: 'chat_send',
      outcome,
      durationMs: performance.now() - started,
    });
  }

  clearPending(id: string): void {
    this.finishSend(id, 'superseded');
    const timer = this.pending.get(id);
    if (timer !== undefined) clearTimeout(timer);
    this.pending.delete(id);
    this.retrying.delete(id);
  }

  /** Answer a message: the next send quotes it. */
  startReply(message: ChatEntry): void {
    this.replyingTo = message;
    const cancel = button('✕', () => this.cancelReply(), 'reply-bar-cancel');
    cancel.setAttribute('aria-label', 'Cancel reply');
    this.replyBar.replaceChildren(
      el('span', `Replying to ${message.participantName}`, 'reply-bar-label'),
      el('span', excerpt(message.content), 'reply-bar-excerpt'),
      cancel,
    );
    this.replyBar.hidden = false;
    this.context.elements.input.focus();
  }

  cancelReply(): void {
    this.replyingTo = null;
    this.replyBar.hidden = true;
    this.replyBar.replaceChildren();
  }

  updateComposerMode(): void {
    this.touchComposer = this.touchPointer.matches || navigator.maxTouchPoints > 0;
    this.context.elements.sendButton.hidden = !this.touchComposer;
    const help = this.touchComposer
      ? 'Enter adds a new line. Tap Send or press Ctrl/Cmd+Enter to send.'
      : 'Enter to send. Shift+Enter for a new line.';
    this.context.elements.input.title = help;
    this.context.elements.input.setAttribute('aria-description', help);
    this.context.elements.input.setAttribute('enterkeyhint', this.touchComposer ? 'enter' : 'send');
    this.resizeInput();
  }

  /** Let CSS cap the height at six lines; remeasure after every draft or width change. */
  resizeInput(): void {
    if (!this.context.elements.input.clientWidth) return;
    this.context.elements.input.style.setProperty('height', 'auto');
    if (this.context.elements.input.value) {
      const border =
        this.context.elements.input.offsetHeight - this.context.elements.input.clientHeight;
      this.context.elements.input.style.setProperty(
        'height',
        `${this.context.elements.input.scrollHeight + border}px`,
      );
    }
    document
      .getElementById('chat-panel')!
      .style.setProperty(
        '--chat-composer-height',
        `${this.context.elements.inputRow.offsetHeight}px`,
      );
    if (this.context.seenAtBottom)
      this.context.messages.scrollTop = this.context.messages.scrollHeight;
  }

  onKey(event: KeyboardEvent): void {
    if (this.events.signal.aborted) return;
    // Safari may finish composition just before dispatching the confirming Enter.
    // oxlint-disable-next-line typescript/no-deprecated
    if (event.isComposing || event.keyCode === 229) return;
    const sendKey =
      event.key === 'Enter' &&
      !event.shiftKey &&
      !event.altKey &&
      (!this.touchComposer || event.ctrlKey || event.metaKey);
    if (this.mentions.handleKey(event, sendKey)) return;
    if (event.key === 'Escape' && this.replyingTo) {
      event.preventDefault();
      this.cancelReply();
      return;
    }
    if (sendKey) {
      event.preventDefault();
      this.mentions.closeMentions();
      this.send();
    }
    if (
      event.key === 'ArrowUp' &&
      (this.context.elements.input.value === '' ||
        this.composition.isRecalling(this.context.store.active))
    ) {
      event.preventDefault();
      this.context.elements.input.value = this.composition.recall(
        this.context.store.active,
        'up',
        this.context.elements.input.value,
      );
      this.resizeInput();
    } else if (
      event.key === 'ArrowDown' &&
      this.composition.isRecalling(this.context.store.active)
    ) {
      event.preventDefault();
      this.context.elements.input.value = this.composition.recall(
        this.context.store.active,
        'down',
        this.context.elements.input.value,
      );
      this.resizeInput();
    }
  }

  insertText(text: string, start: number, end: number): void {
    if (this.events.signal.aborted) return;
    const next =
      this.context.elements.input.value.slice(0, start) +
      text +
      this.context.elements.input.value.slice(end);
    if (next.length > 2000 || new TextEncoder().encode(next).length > 4096) {
      this.context.options.notify('Message is too long');
      return;
    }
    this.context.elements.input.setRangeText(text, start, end, 'end');
    this.composition.save(this.context.store.active, this.context.elements.input.value);
    this.resizeInput();
  }
}
