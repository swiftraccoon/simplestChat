import { MediaControls } from '../src/media-controls';
import { createRoomMedia } from '../src/screens/room-media';
import { observeVideoLayout } from '../src/video-layout';
import { createUiTasks } from '../src/ui-tasks';

// Use the production renderer and controls with synthetic tracks. Detached
// publishing controls keep the page's existing room controller as their owner.
const previousGrid = document.querySelector('#video-grid');
const videoGrid = previousGrid.cloneNode(false);
previousGrid.replaceWith(videoGrid);
const mediaControls = new MediaControls({ getRoom: () => null, notify: () => {} });
const uiTasks = createUiTasks({ owner: () => ({ room: null, accountId: null }), notify: () => {} });
const media = createRoomMedia(
  {
    videoGrid,
    micBtn: document.createElement('button'),
    camBtn: document.createElement('button'),
    screenBtn: document.createElement('button'),
    nameInput: document.createElement('input'),
    roomMoreRepair: document.createElement('div'),
    roomNavigation: document.createElement('div'),
  },
  { room: null },
  { mediaControls, telemetry: { record() {} }, ...uiTasks },
  {
    personalMicMode: 'open',
    micMode: 'open',
    pttHeld: false,
    pttActivation: 0,
    cameraTogglePending: false,
    microphoneTogglePending: false,
  },
  { recovering: false, followedSpeaker: null, talkRow: null, showModerationMenu() {} },
);
const stopVideoLayout = observeVideoLayout(videoGrid);
const videos = new Map();
let audioContext;

window.__mediaLayout = {
  add(id, width, height, screen = false) {
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const context = canvas.getContext('2d');
    const draw = () => {
      context.fillStyle = screen ? '#366082' : '#4b7350';
      context.fillRect(0, 0, canvas.width, canvas.height);
      context.fillStyle = 'white';
      context.font = '28px sans-serif';
      context.fillText(id, 20, 45);
    };
    draw();
    const track = canvas.captureStream(10).getVideoTracks()[0];
    const timer = setInterval(draw, 100);
    videos.set(id, { canvas, track, timer, screen, key: screen ? id + ':screen' : id });
    media.renderRemoteTrack(id, id, track, 'video', screen ? 'screen' : 'camera');
  },
  addAudio(id) {
    audioContext ??= new AudioContext();
    const destination = audioContext.createMediaStreamDestination();
    const track = destination.stream.getAudioTracks()[0];
    videos.set(id, { track, destination, screen: false, key: id });
    media.renderRemoteTrack(id, id, track, 'audio', 'microphone');
  },
  rotate(id, width, height) {
    const { canvas } = videos.get(id);
    canvas.width = width;
    canvas.height = height;
  },
  remove(id) {
    const entry = videos.get(id);
    clearInterval(entry.timer);
    entry.track.stop();
    media.removeRemoteTrack(
      id,
      'fixture-producer',
      entry.track.kind,
      entry.screen ? 'screen' : 'camera',
    );
    videos.delete(id);
  },
  pin(id) {
    media.setPinnedTile(id === null ? null : videos.get(id).key);
  },
  async close() {
    stopVideoLayout();
    media.dispose();
    mediaControls.destroy();
    for (const { track, timer } of videos.values()) {
      clearInterval(timer);
      track.stop();
    }
    videos.clear();
    if (audioContext) await audioContext.close();
  },
};
