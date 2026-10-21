import type { Elements } from '../dom';
import type { RoomScreenStore, RoomScreenServices } from './room';
import type { ScreenShareResult } from '../media';
import type { VideoDeferredReason } from '../receive-policy';
import { mediaErrorMessage } from '../media-controls';
import { observeFirstVideoFrame } from '../media-telemetry';
import { spatialLayerForRenderedWidth } from '../layer-cap';
import { avatarColors, chatColor } from '../avatar-colors';
import * as icons from '../icons';
import { button, el } from '../ui';
import { createScope } from '../disposable';
import type { segmentRow } from '../settings-sheet';
export type MicMode = 'open' | 'ptt';
export interface RoomPublishingState {
  personalMicMode: MicMode;
  micMode: MicMode;
  pttHeld: boolean;
  pttActivation: number;
  cameraTogglePending: boolean;
  microphoneTogglePending: boolean;
}
interface RoomMediaContext {
  readonly recovering: boolean;
  readonly followedSpeaker: string | null;
  readonly talkRow: ReturnType<typeof segmentRow> | null;
  showModerationMenu(targetId: string, targetName: string, x: number, y: number): void;
}

/** Owns publishing actions and the rendered media resources for one app lifetime. */
export function createRoomMedia(
  elements: Elements,
  store: RoomScreenStore,
  services: RoomScreenServices,
  publishing: RoomPublishingState,
  context: RoomMediaContext,
) {
  const scope = createScope();
  const { micBtn, camBtn, screenBtn, nameInput, videoGrid, roomMoreRepair } = elements;
  function listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement | Document | Window,
    type: K,
    listener: (event: HTMLElementEventMap[K]) => void,
    options: AddEventListenerOptions = {},
  ): void {
    target.addEventListener(type, listener as EventListener, { ...options, signal: scope.signal });
  }

  const remoteTiles = new Map<string, HTMLDivElement>();

  let pinnedTileKey: string | null = null;

  let pictureInPictureVideo: HTMLVideoElement | null = null;

  const tileVisibility =
    typeof IntersectionObserver === 'undefined'
      ? null
      : new IntersectionObserver(
          (entries) => {
            if (scope.signal.aborted) return;
            for (const entry of entries) {
              const tile = entry.target as HTMLElement;
              if (!tile.isConnected) continue;
              const participantId = tile.dataset['participantId'];
              if (participantId)
                store.room?.setRemoteVideoVisibility(
                  participantId,
                  tile.dataset['videoSource'],
                  entry.isIntersecting,
                );
            }
          },
          { threshold: 0.01 },
        );

  /** Size observers per camera tile: the layer a tile can show caps what it requests. */
  const tileLayerCaps = new Map<string, { observer: ResizeObserver; layer: number | null }>();

  function observeTileSize(tileKey: string, participantId: string, video: HTMLVideoElement): void {
    if (tileLayerCaps.has(tileKey) || typeof ResizeObserver === 'undefined') return;
    const owner = store.room;
    const membership = owner?.membershipVersion;
    const state: { observer: ResizeObserver; layer: number | null } = {
      observer: new ResizeObserver((entries) => {
        if (
          scope.signal.aborted ||
          tileLayerCaps.get(tileKey) !== state ||
          store.room !== owner ||
          owner?.membershipVersion !== membership
        )
          return;
        const width = entries[0]?.contentRect.width ?? video.clientWidth;
        const layer = spatialLayerForRenderedWidth(width, window.devicePixelRatio, state.layer);
        if (layer === state.layer) return;
        state.layer = layer;
        store.room?.setRemoteVideoSizeCap(participantId, layer);
      }),
      layer: null,
    };
    state.observer.observe(video);
    tileLayerCaps.set(tileKey, state);
  }

  function stopObservingTileSize(tileKey: string): void {
    const state = tileLayerCaps.get(tileKey);
    if (!state) return;
    state.observer.disconnect();
    tileLayerCaps.delete(tileKey);
  }

  // --- Control buttons ---
  function updateMicButton(enabled: boolean): void {
    if (scope.signal.aborted) return;
    let tooltip: string;
    if (publishing.micMode === 'ptt') {
      tooltip = enabled ? 'Release to mute' : 'Hold Space/T to talk';
    } else {
      tooltip = enabled ? 'Turn off mic (M)' : 'Turn on mic (M)';
    }
    services.setButtonContent(micBtn, enabled ? icons.micOn() : icons.micOff(), tooltip);
    micBtn.classList.toggle('active', enabled);
    micBtn.classList.toggle('muted', !enabled);
    micBtn.classList.toggle('ptt-active', publishing.micMode === 'ptt' && enabled);

    // PTT mode label under mic button
    let pttLabel = micBtn.querySelector('.ptt-label');
    if (publishing.micMode === 'ptt') {
      if (!pttLabel) {
        pttLabel = document.createElement('span');
        pttLabel.className = 'ptt-label';
        micBtn.appendChild(pttLabel);
      }
      pttLabel.textContent = 'PTT';
    } else {
      pttLabel?.remove();
    }
  }

  function updateCamButton(enabled: boolean): void {
    if (scope.signal.aborted) return;
    services.setButtonContent(
      camBtn,
      enabled ? icons.camOn() : icons.camOff(),
      enabled ? 'Turn off camera (V)' : 'Turn on camera (V)',
    );
    camBtn.classList.toggle('active', enabled);
    camBtn.classList.toggle('muted', !enabled);
  }

  function screenAudioLabel(): string {
    switch (store.room?.screenShareAudio) {
      case 'sharing':
        return 'screen audio included';
      case 'failed':
        return 'screen audio failed';
      case 'ended':
        return 'screen audio ended';
      case undefined:
      case 'off':
      case 'not_provided':
        return 'no screen audio';
    }
  }

  function updateScreenButton(active: boolean): void {
    if (scope.signal.aborted) return;
    services.setButtonContent(
      screenBtn,
      active ? icons.screenShareOff() : icons.screenShare(),
      active ? `Stop Sharing (S) — ${screenAudioLabel()}` : 'Screen (S)',
    );
    screenBtn.classList.toggle('active', active);
    screenShareStatus.hidden = !active;
    screenShareStatus.textContent = active ? `Sharing your screen · ${screenAudioLabel()}` : '';
  }

  /** Show/hide/update the local video tile based on current mic+cam state */
  function updateLocalTile(): void {
    if (scope.signal.aborted) return;
    if (!store.room) return;
    const camOn = store.room.videoEnabled;
    const micOn = store.room.audioEnabled;
    const tile = document.getElementById('local-tile');
    const localName = store.room.nickname || nameInput.value.trim();

    if (!camOn && !micOn) {
      // Both off — remove tile entirely
      tile?.remove();
      updateVideoGridCount();
      return;
    }

    if (!tile) {
      // Need to re-create the tile (was removed when both were off)
      const newTile = document.createElement('div');
      newTile.className = 'video-tile local';
      newTile.id = 'local-tile';

      const nameTag = document.createElement('div');
      nameTag.className = 'name-tag';
      nameTag.textContent = `${localName} (You)`;
      newTile.appendChild(nameTag);
      paintTile(newTile, localName, store.room.chatStyle?.color ?? null);

      videoGrid.prepend(newTile);
      updateVideoGridCount();

      if (camOn) {
        // Re-attach video from local stream
        const stream = store.room.getLocalStream();
        if (stream) {
          const video = document.createElement('video');
          video.autoplay = true;
          video.muted = true;
          video.playsInline = true;
          video.srcObject = stream;
          newTile.insertBefore(video, newTile.firstChild);
        }
      } else {
        // Mic on, cam off — show avatar
        addLocalAvatar(newTile, localName);
      }
      return;
    }

    // Tile exists — update it
    if (camOn) {
      // Show video, hide avatar
      const avatar = tile.querySelector('.no-video-avatar') as HTMLElement | null;
      if (avatar) avatar.remove();
      if (!tile.querySelector('video')) {
        const stream = store.room.getLocalStream();
        if (stream) {
          const video = document.createElement('video');
          video.autoplay = true;
          video.muted = true;
          video.playsInline = true;
          video.srcObject = stream;
          tile.insertBefore(video, tile.firstChild);
        }
      }
    } else {
      // Cam off — remove video, show avatar
      const video = tile.querySelector('video');
      if (video) {
        video.srcObject = null;
        video.remove();
      }
      if (!tile.querySelector('.no-video-avatar')) {
        addLocalAvatar(tile, localName);
      }
    }
  }

  function addLocalAvatar(tile: HTMLElement, name: string): void {
    const noVideoAvatar = document.createElement('div');
    noVideoAvatar.className = 'no-video-avatar';
    const initial = document.createElement('div');
    initial.className = 'avatar-initial';
    initial.textContent = name.charAt(0).toUpperCase();
    noVideoAvatar.appendChild(initial);
    tile.insertBefore(noVideoAvatar, tile.firstChild);
    paintTile(tile, name, store.room?.chatStyle?.color ?? null);
  }

  /** A person's chosen palette color, the local person included; null means automatic. */
  function participantColor(participantId: string): string | null {
    if (!store.room) return null;
    const look =
      participantId === store.room.localParticipantId
        ? store.room.chatStyle
        : store.room.getParticipants().get(participantId)?.chatStyle;
    return look?.color ?? null;
  }

  /** A person's color on a tile: the dot beside the name tag and the camera-off initial. */
  function paintTile(tile: HTMLElement, name: string, color: string | null): void {
    tile.style.setProperty('--person-color', chatColor(name, color));
    const initial = tile.querySelector<HTMLElement>('.avatar-initial');
    if (initial) Object.assign(initial.style, avatarColors(name, color));
  }

  /** A changed look or name repaints that person's tiles; the people lists re-render themselves. */
  function repaintTiles(participantId: string): void {
    if (scope.signal.aborted) return;
    if (!store.room) return;
    if (participantId === store.room.localParticipantId) {
      const tile = document.getElementById('local-tile');
      if (tile)
        paintTile(
          tile,
          store.room.nickname || nameInput.value.trim(),
          participantColor(participantId),
        );
      return;
    }
    const name = store.room.getParticipants().get(participantId)?.name;
    if (name === undefined) return;
    for (const tile of remoteTiles.values())
      if (tile.dataset['participantId'] === participantId)
        paintTile(tile, name, participantColor(participantId));
  }

  function handleLocalCaptureStopped(kind: 'audio' | 'video'): void {
    if (scope.signal.aborted) return;
    if (!store.room) return;
    if (kind === 'audio') {
      // Retire the held intent and any pending activation; only a new user action may recapture.
      publishing.pttActivation++;
      publishing.pttHeld = false;
      services.showToast(
        publishing.micMode === 'ptt'
          ? 'Microphone stopped. Release, then hold Space/T or the microphone button again to restart.'
          : 'Microphone stopped. Press M or the mic button to turn it back on.',
      );
    } else {
      services.showToast('Camera stopped. Press V or the camera button to turn it back on.');
    }
  }

  /** The broadcast stays on, so viewers see it resume with the camera. */
  function handleLocalVideoStalled(stalled: boolean): void {
    if (scope.signal.aborted) return;
    if (stalled)
      services.showToast(
        'Your camera is not sending video. Check that it is connected and not in use by another app; your broadcast resumes when it does.',
        10_000,
        'error',
      );
    else services.showToast('Your camera is sending video again.');
  }

  async function pttActivate(): Promise<void> {
    if (scope.signal.aborted) return;
    if (!store.room || !store.room.hasMedia || publishing.pttHeld) return;
    const activeRoom = store.room;
    const membership = activeRoom.membershipVersion;
    const activation = ++publishing.pttActivation;
    publishing.pttHeld = true;
    try {
      await activeRoom.unmuteAudio();
      if (
        scope.signal.aborted ||
        activation !== publishing.pttActivation ||
        store.room !== activeRoom ||
        membership !== activeRoom.membershipVersion ||
        !publishing.pttHeld
      )
        return;
      updateMicButton(activeRoom.audioEnabled);
      updateLocalTile();
    } catch (error) {
      if (
        scope.signal.aborted ||
        activation !== publishing.pttActivation ||
        store.room !== activeRoom ||
        membership !== activeRoom.membershipVersion ||
        !publishing.pttHeld
      )
        return;
      publishing.pttHeld = false;
      activeRoom.muteAudio();
      updateMicButton(false);
      updateLocalTile();
      services.showToast(mediaErrorMessage(error, 'microphone'));
    }
  }

  function pttDeactivate(): void {
    if (!store.room || !publishing.pttHeld) return;
    publishing.pttActivation++;
    publishing.pttHeld = false;
    store.room.muteAudio();
    updateMicButton(false);
    updateLocalTile();
  }

  listen(micBtn, 'mousedown', (e) => {
    if (!store.room || !store.room.hasMedia) return;
    if (publishing.micMode === 'ptt' && canStartBroadcast('microphone')) {
      e.preventDefault(); // prevent focus loss
      services.observeUiTask(pttActivate(), 'Could not enable push-to-talk');
    }
  });

  listen(micBtn, 'mouseup', () => {
    if (publishing.micMode === 'ptt') pttDeactivate();
  });

  listen(micBtn, 'mouseleave', () => {
    if (publishing.micMode === 'ptt') pttDeactivate();
  });

  listen(
    micBtn,
    'click',
    services.asyncUiAction(async () => {
      if (publishing.micMode === 'open') await toggleMicrophone();
      // In PTT mode, click is handled by mousedown/mouseup above
    }, 'Could not update the microphone'),
  );

  function canStartBroadcast(kind: 'microphone' | 'camera' | 'screen'): boolean {
    if (scope.signal.aborted || !store.room?.hasMedia || context.recovering) return false;
    const settings = store.room.roomSettings;
    const hasVoice = ['member', 'moderator', 'admin', 'owner'].includes(store.room.role);
    if (settings?.moderated && !hasVoice) {
      services.showToast('Raise your hand to request broadcasting permission');
      return false;
    }
    if (settings?.guestsCanBroadcast === false && store.room.role === 'guest') {
      services.showToast('Guests cannot broadcast in this room');
      return false;
    }
    if (kind === 'camera' && settings?.allowVideo === false) return false;
    if (
      kind === 'screen' &&
      (settings?.allowScreenSharing === false || !navigator.mediaDevices?.getDisplayMedia)
    )
      return false;
    return true;
  }

  async function toggleMicrophone(): Promise<void> {
    if (scope.signal.aborted) return;
    const activeRoom = store.room;
    if (
      !activeRoom?.hasMedia ||
      publishing.microphoneTogglePending ||
      publishing.micMode !== 'open'
    )
      return;
    if (!activeRoom.audioEnabled && !canStartBroadcast('microphone')) return;
    const membership = activeRoom.membershipVersion;
    publishing.microphoneTogglePending = true;
    try {
      const enabled = await activeRoom.toggleAudio();
      if (
        scope.signal.aborted ||
        store.room !== activeRoom ||
        membership !== activeRoom.membershipVersion
      )
        return;
      updateMicButton(enabled);
      updateLocalTile();
    } catch (error) {
      if (
        scope.signal.aborted ||
        store.room !== activeRoom ||
        membership !== activeRoom.membershipVersion
      )
        return;
      updateMicButton(activeRoom.audioEnabled);
      services.showToast(mediaErrorMessage(error, 'microphone'));
    } finally {
      publishing.microphoneTogglePending = false;
    }
  }

  async function toggleCamera(): Promise<void> {
    if (scope.signal.aborted) return;
    const activeRoom = store.room;
    if (!activeRoom?.hasMedia || publishing.cameraTogglePending) return;
    if (!activeRoom.videoEnabled && !canStartBroadcast('camera')) return;
    const membership = activeRoom.membershipVersion;
    publishing.cameraTogglePending = true;
    try {
      const enabled = await activeRoom.toggleVideo();
      if (
        scope.signal.aborted ||
        store.room !== activeRoom ||
        membership !== activeRoom.membershipVersion
      )
        return;
      updateCamButton(enabled);
      updateLocalTile();
    } catch (error) {
      if (
        scope.signal.aborted ||
        store.room !== activeRoom ||
        membership !== activeRoom.membershipVersion
      )
        return;
      updateCamButton(activeRoom.videoEnabled);
      services.showToast(mediaErrorMessage(error, 'camera'));
    } finally {
      publishing.cameraTogglePending = false;
    }
  }

  const refreshMediaBtn = button(
    'Refresh incoming media',
    () => {
      const owner = store.room;
      if (!owner?.connected || refreshMediaBtn.disabled) return;
      const membership = owner.membershipVersion;
      refreshMediaBtn.disabled = true;
      refreshMediaBtn.setAttribute('aria-busy', 'true');
      services.observeUiTask(
        (async () => {
          try {
            await owner.refreshIncomingMedia();
            if (
              !scope.signal.aborted &&
              store.room === owner &&
              owner.membershipVersion === membership
            )
              services.showToast(
                'Incoming subscriptions refreshed. Your microphone and camera settings are unchanged.',
              );
          } catch (error) {
            if (
              !scope.signal.aborted &&
              store.room === owner &&
              owner.membershipVersion === membership
            )
              services.showToast(
                error instanceof Error ? error.message : 'Incoming media could not be refreshed',
                7000,
              );
          } finally {
            refreshMediaBtn.disabled = false;
            refreshMediaBtn.setAttribute('aria-busy', 'false');
          }
        })(),
        'Could not refresh incoming media',
      );
    },
    'btn-secondary',
  );

  refreshMediaBtn.id = 'refresh-incoming-media';

  roomMoreRepair.prepend(refreshMediaBtn);

  const screenShareStatus = el('span', '', 'settings-description');

  screenShareStatus.id = 'screen-share-status';

  screenShareStatus.setAttribute('role', 'status');

  screenShareStatus.hidden = true;

  elements.roomNavigation.appendChild(screenShareStatus);

  const screenShareFailure: Record<
    Extract<ScreenShareResult, { status: 'not_started' }>['reason'],
    string
  > = {
    cancelled_or_denied:
      'Screen sharing was cancelled or permission was denied. Try again to choose a screen, window, or tab.',
    unavailable: 'Screen sharing is not supported in this browser.',
    not_ready: 'The call is reconnecting. Try sharing when it is ready.',
    busy: 'A screen-sharing request is already in progress.',
    superseded: 'Screen sharing was cancelled because the room changed.',
    not_readable:
      'Your browser could not access that screen. Check screen-recording permission in your system settings.',
    no_source: 'No shareable screen or window was available.',
    invalid_state: 'Return to this tab and click Screen to choose what to share.',
    failed: 'Screen sharing could not start. Try choosing a source again.',
  };

  async function toggleScreenShare(): Promise<void> {
    if (scope.signal.aborted) return;
    const activeRoom = store.room;
    if (!activeRoom?.hasMedia) return;
    const membership = activeRoom.membershipVersion;
    if (activeRoom.isScreenSharing) {
      activeRoom.stopScreenShare();
      updateScreenButton(false);
    } else if (canStartBroadcast('screen')) {
      const result = await activeRoom.startScreenShare();
      if (
        scope.signal.aborted ||
        store.room !== activeRoom ||
        membership !== activeRoom.membershipVersion
      )
        return;
      updateScreenButton(activeRoom.isScreenSharing);
      if (result.status === 'not_started')
        services.showToast(screenShareFailure[result.reason], 7000);
      else if (result.audio === 'failed')
        services.showToast(
          'Your screen is shared, but its audio could not start. Stop and share again to retry audio.',
          7000,
        );
    }
  }

  listen(camBtn, 'click', services.asyncUiAction(toggleCamera, 'Could not update the camera'));

  listen(
    screenBtn,
    'click',
    services.asyncUiAction(toggleScreenShare, 'Could not update screen sharing'),
  );

  listen(
    micBtn,
    'touchstart',
    (event) => {
      if (publishing.micMode !== 'ptt') return;
      event.preventDefault();
      if (canStartBroadcast('microphone'))
        services.observeUiTask(pttActivate(), 'Could not enable push-to-talk');
    },
    { passive: false },
  );

  listen(micBtn, 'touchend', () => pttDeactivate());

  listen(micBtn, 'touchcancel', () => pttDeactivate());

  function setMicMode(mode: MicMode, remember = true): void {
    if (remember && store.room?.roomSettings?.pushToTalk && mode !== 'ptt') {
      context.talkRow?.set('ptt');
      services.showToast('This room requires push to talk');
      return;
    }
    if (remember) {
      publishing.personalMicMode = mode;
      services.writeLocalPreference('micMode', mode);
    }
    const effectiveMode = store.room?.roomSettings?.pushToTalk ? 'ptt' : mode;
    const changed = publishing.micMode !== effectiveMode;
    publishing.micMode = effectiveMode;
    context.talkRow?.set(effectiveMode);

    if (store.room && store.room.hasMedia) {
      if (
        effectiveMode === 'ptt' &&
        (changed || (!publishing.pttHeld && store.room.audioEnabled))
      ) {
        // Entering PTT mode — mute immediately
        publishing.pttHeld = false;
        publishing.pttActivation++;
        store.room.muteAudio();
        updateMicButton(false);
        updateLocalTile();
      } else if (changed) {
        // Entering open mic mode — reset PTT state, leave mic as-is (muted)
        publishing.pttHeld = false;
        publishing.pttActivation++;
        updateMicButton(store.room.audioEnabled);
      }
      updateMicButton(store.room.audioEnabled);
    }
  }

  // --- Update video grid count for adaptive sizing ---
  /** Pin one tile first and highlight it without resizing any cells. This viewer only. */
  function setPinnedTile(key: string | null): void {
    pinnedTileKey = key !== null && remoteTiles.has(key) ? key : null;
    for (const [tileKey, tile] of remoteTiles) {
      const pinned = tileKey === pinnedTileKey;
      tile.classList.toggle('pinned', pinned);
      const pin = tile.querySelector('.tile-pin');
      if (pin) {
        pin.setAttribute('aria-pressed', String(pinned));
        pin.textContent = pinned ? 'Unpin' : 'Pin';
      }
    }
    videoGrid.classList.toggle('has-pinned', pinnedTileKey !== null);
    orderVideoTiles();
    const tile = pinnedTileKey === null ? null : remoteTiles.get(pinnedTileKey);
    store.room?.setPinnedRemoteVideo(
      tile?.dataset['participantId'] ?? null,
      tile?.dataset['videoSource'],
    );
  }

  function orderVideoTiles(): void {
    for (const [key, tile] of remoteTiles) {
      tile.style.order =
        key === pinnedTileKey
          ? '-3'
          : context.followedSpeaker !== null && tile.dataset['videoSource'] === 'screen'
            ? '-2'
            : key === context.followedSpeaker
              ? '-1'
              : '';
    }
  }

  function updateVideoGridCount(): void {
    orderVideoTiles();
    if (pinnedTileKey !== null && !remoteTiles.has(pinnedTileKey)) setPinnedTile(null);
    const count = videoGrid.children.length;
    if (count <= 1) videoGrid.dataset['count'] = '1';
    else if (count === 2) videoGrid.dataset['count'] = '2';
    else if (count <= 4) videoGrid.dataset['count'] = '4';
    else if (count <= 6) videoGrid.dataset['count'] = '6';
    else videoGrid.dataset['count'] = 'many';
  }

  function ensureRemoteTile(
    participantId: string,
    participantName: string,
    source?: string,
  ): HTMLDivElement {
    const isScreen = source === 'screen' || source === 'screen-audio';
    const tileKey = isScreen ? `${participantId}:screen` : participantId;
    let tile = remoteTiles.get(tileKey);

    if (!tile) {
      tile = document.createElement('div');
      tile.className = isScreen ? 'video-tile screen-share' : 'video-tile';
      tile.dataset['participantId'] = participantId;
      if (isScreen) tile.dataset['videoSource'] = 'screen';

      // Context menu for moderation on remote tiles
      tile.addEventListener('contextmenu', (e) => {
        e.preventDefault();
        context.showModerationMenu(participantId, participantName, e.clientX, e.clientY);
      });

      // No-video avatar (not shown for screen share tiles)
      if (!isScreen) {
        const noVideoAvatar = document.createElement('div');
        noVideoAvatar.className = 'no-video-avatar';
        const initial = document.createElement('div');
        initial.className = 'avatar-initial';
        initial.textContent = participantName.charAt(0).toUpperCase();
        noVideoAvatar.appendChild(initial);
        tile.appendChild(noVideoAvatar);
      }

      const nameTag = document.createElement('div');
      nameTag.className = 'name-tag';
      nameTag.textContent = isScreen ? `${participantName} (Screen)` : participantName;
      tile.appendChild(nameTag);
      paintTile(tile, participantName, participantColor(participantId));
      const pin = document.createElement('button');
      pin.type = 'button';
      pin.className = 'tile-pin';
      pin.textContent = 'Pin';
      pin.setAttribute('aria-pressed', 'false');
      pin.setAttribute(
        'aria-label',
        `Pin ${isScreen ? `${participantName}'s screen` : participantName}`,
      );
      pin.addEventListener('click', () =>
        setPinnedTile(pinnedTileKey === tileKey ? null : tileKey),
      );
      tile.appendChild(pin);

      remoteTiles.set(tileKey, tile);
      videoGrid.appendChild(tile);
      updateVideoGridCount();
      tileVisibility?.observe(tile);
    }

    return tile;
  }

  function renderDeferredVideo(
    participantId: string,
    participantName: string,
    source: string | undefined,
    reason: VideoDeferredReason,
  ): void {
    if (scope.signal.aborted) return;
    const tile = ensureRemoteTile(participantId, participantName, source);
    tile.dataset['videoDeferred'] = 'true';
    const video = tile.querySelector('video');
    if (video) {
      video.srcObject = null;
      video.remove();
    }
    stopObservingTileSize(source === 'screen' ? `${participantId}:screen` : participantId);
    const avatar = tile.querySelector<HTMLElement>('.no-video-avatar');
    if (avatar) avatar.style.display = 'none';
    let notice = tile.querySelector<HTMLElement>('.video-deferred-notice');
    if (!notice) {
      notice = document.createElement('div');
      notice.className = 'video-deferred-notice';
      tile.append(notice);
    }
    const reasons = {
      'audio-only': 'Audio only',
      offscreen: 'Video resumes when visible',
      limit: 'Pin to watch this video',
      hidden: 'Video hidden for you',
    };
    notice.textContent = reasons[reason];
    services.mediaControls.attachTile(tile, participantId, participantName);
  }

  function renderRemoteTrack(
    participantId: string,
    participantName: string,
    track: MediaStreamTrack,
    _kind: 'audio' | 'video',
    source?: string,
  ): void {
    if (scope.signal.aborted) return;
    const isScreen = source === 'screen' || source === 'screen-audio';
    const tileKey = isScreen ? `${participantId}:screen` : participantId;
    const tile = ensureRemoteTile(participantId, participantName, source);
    if (track.kind === 'video') {
      delete tile.dataset['videoDeferred'];
      tile.querySelector('.video-deferred-notice')?.remove();
      let video = tile.querySelector('video');
      if (!video) {
        video = document.createElement('video');
        video.autoplay = true;
        video.playsInline = true;
        const ownedVideo = video;
        video.addEventListener('enterpictureinpicture', () => {
          if (remoteTiles.get(tileKey) !== tile || tile.querySelector('video') !== ownedVideo)
            return;
          pictureInPictureVideo = ownedVideo;
          store.room?.setPictureInPictureVideo(participantId, source);
        });
        video.addEventListener('leavepictureinpicture', () => {
          if (pictureInPictureVideo !== ownedVideo) return;
          pictureInPictureVideo = null;
          store.room?.setPictureInPictureVideo(null);
        });
        tile.insertBefore(video, tile.firstChild);
      }
      observeFirstVideoFrame(video, services.telemetry.record);
      video.srcObject = new MediaStream([track]);
      if (!isScreen) observeTileSize(tileKey, participantId, video);
      const avatar = tile.querySelector('.no-video-avatar') as HTMLElement | null;
      if (avatar) avatar.style.display = 'none';
    } else {
      let audio = tile.querySelector('audio');
      if (!audio) {
        audio = document.createElement('audio');
        audio.autoplay = true;
        tile.appendChild(audio);
      }
      audio.srcObject = new MediaStream([track]);
    }
    services.mediaControls.attachTile(tile, participantId, participantName);
  }

  function removeRemoteTrack(
    participantId: string,
    _producerId: string,
    kind: 'audio' | 'video',
    source?: string,
  ): void {
    if (scope.signal.aborted) return;
    const isScreen = source === 'screen' || source === 'screen-audio';
    const tileKey = isScreen ? `${participantId}:screen` : participantId;
    const tile = remoteTiles.get(tileKey);
    if (!tile) return;

    if (kind === 'video') {
      delete tile.dataset['videoDeferred'];
      tile.querySelector('.video-deferred-notice')?.remove();
      stopObservingTileSize(tileKey);
      const video = tile.querySelector('video');
      if (video) {
        video.srcObject = null;
        video.remove();
      }
      const avatar = tile.querySelector('.no-video-avatar') as HTMLElement | null;
      if (avatar) avatar.style.display = '';
    } else {
      const audio = tile.querySelector('audio');
      if (audio) {
        audio.srcObject = null;
        audio.remove();
      }
    }

    // Remove tile entirely if no active media remains
    if (
      !tile.querySelector('video') &&
      !tile.querySelector('audio') &&
      !tile.dataset['videoDeferred']
    ) {
      stopObservingTileSize(tileKey);
      services.mediaControls.detachTile(tile);
      tileVisibility?.unobserve(tile);
      tile.remove();
      remoteTiles.delete(tileKey);
      updateVideoGridCount();
    }
  }
  function mediaElements(): HTMLMediaElement[] {
    return [...remoteTiles.values()].flatMap((tile) => [
      ...tile.querySelectorAll<HTMLMediaElement>('audio, video'),
    ]);
  }
  function playbackElements(): HTMLMediaElement[] {
    return [...remoteTiles.values()]
      .filter((tile) => !tile.classList.contains('personal-media-hidden'))
      .flatMap((tile) => [...tile.querySelectorAll<HTMLMediaElement>('audio, video')]);
  }
  /** Retire render observers and stream references before another membership uses the tiles. */
  function resetTiles(): void {
    for (const key of tileLayerCaps.keys()) stopObservingTileSize(key);
    tileVisibility?.disconnect();
    for (const element of mediaElements()) element.srcObject = null;
    for (const tile of remoteTiles.values()) tile.remove();
    pictureInPictureVideo = null;
    remoteTiles.clear();
    setPinnedTile(null);
  }
  scope.onDispose(() => {
    pttDeactivate();
    resetTiles();
    refreshMediaBtn.remove();
    screenShareStatus.remove();
  });
  return {
    tiles: remoteTiles,
    visibility: tileVisibility,
    stopObservingTileSize,
    updateMicButton,
    updateCamButton,
    updateScreenButton,
    updateLocalTile,
    repaintTiles,
    handleLocalCaptureStopped,
    handleLocalVideoStalled,
    pttActivate,
    pttDeactivate,
    canStartBroadcast,
    toggleMicrophone,
    toggleCamera,
    toggleScreenShare,
    setMicMode,
    setPinnedTile,
    orderVideoTiles,
    updateVideoGridCount,
    renderDeferredVideo,
    renderRemoteTrack,
    removeRemoteTrack,
    clearPictureInPicture: () => {
      pictureInPictureVideo = null;
    },
    refreshIncomingMedia: () => refreshMediaBtn.click(),
    get canRefreshIncomingMedia() {
      return !scope.signal.aborted && !refreshMediaBtn.disabled;
    },
    mediaElements,
    playbackElements,
    resetTiles,
    dispose: () => scope.dispose(),
  };
}
