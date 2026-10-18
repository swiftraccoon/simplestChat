// Owned display content and a pixel reader for actual decoded text. Kept free
// of browser setup so the reader can also reject corrupt/blank frames in Node.
const text = 'SLIDES 1234';
const glyphs = {
  S: ['01111', '10000', '10000', '01110', '00001', '00001', '11110'],
  L: ['10000', '10000', '10000', '10000', '10000', '10000', '11111'],
  I: ['11111', '00100', '00100', '00100', '00100', '00100', '11111'],
  D: ['11110', '10001', '10001', '10001', '10001', '10001', '11110'],
  E: ['11111', '10000', '10000', '11110', '10000', '10000', '11111'],
  ' ': ['00000', '00000', '00000', '00000', '00000', '00000', '00000'],
  1: ['00100', '01100', '00100', '00100', '00100', '00100', '01110'],
  2: ['01110', '10001', '00001', '00010', '00100', '01000', '11111'],
  3: ['11110', '00001', '00001', '01110', '00001', '00001', '11110'],
  4: ['00010', '00110', '01010', '10010', '11111', '00010', '00010'],
};

/** Self contained for page.addInitScript; only explicit getDisplayMedia calls
 * start this owned canvas. Camera/microphone still use the engine's fake native
 * devices and every capture request is counted. */
function installPresentationCapture({ glyphs, text }) {
  const evidence = (window.__presentation = { captures: 0, displays: 0, tracks: [], canvas: null });
  const capture = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
  navigator.mediaDevices.getUserMedia = async (constraints) => {
    evidence.captures++;
    return capture(constraints);
  };
  navigator.mediaDevices.getDisplayMedia = async (constraints) => {
    evidence.displays++;
    const canvas = document.createElement('canvas');
    canvas.width = 1920;
    canvas.height = 1080;
    const context = canvas.getContext('2d', { alpha: false });
    let tick = 0;
    function draw() {
      context.fillStyle = '#fff';
      context.fillRect(0, 0, 1920, 1080);
      context.fillStyle = '#000';
      for (let character = 0; character < text.length; character++)
        for (let row = 0; row < 7; row++)
          for (let column = 0; column < 5; column++)
            if (glyphs[text[character]][row][column] === '1')
              context.fillRect(144 + character * 144 + column * 24, 144 + row * 24, 24, 24);
      context.font = '48px sans-serif';
      context.fillText('Owned presentation: text must survive the encoded video path', 144, 460);
      // Changing fine detail makes the motion profile use real bandwidth;
      // a static slide alone can fit the weak link at full resolution.
      let seed = ++tick * 7919;
      for (let y = 600; y < 1080; y += 12)
        for (let x = 0; x < 1920; x += 12) {
          seed = (Math.imul(seed, 1664525) + 1013904223) | 0;
          context.fillStyle = `rgb(${seed & 255},${(seed >>> 8) & 255},${(seed >>> 16) & 255})`;
          context.fillRect(x, y, 12, 12);
        }
    }
    draw();
    const rate = constraints?.video?.frameRate?.max === 30 ? 30 : 15;
    const stream = canvas.captureStream(rate);
    const track = stream.getVideoTracks()[0];
    const timer = setInterval(draw, 1000 / rate);
    const stop = track.stop.bind(track);
    track.stop = () => {
      clearInterval(timer);
      stop();
    };
    track.addEventListener('ended', () => clearInterval(timer), { once: true });
    evidence.tracks.push(track);
    evidence.canvas = canvas;
    return stream;
  };
}

/** With no supplied pixels, reads the real remote screen element. Glyph cell
 * centers survive the 1/4 layer (6 pixels per cell). No packet counter or local
 * source canvas can satisfy this read. The result contains only decoded text,
 * contrast and public video dimensions, never raw pixels or native IDs. */
function decodePresentationFrame({ glyphs, text, pixels, width, height }) {
  if (!pixels) {
    const video = document.querySelector('.video-tile.screen-share:not(.local) video');
    if (!video || video.readyState < 2 || video.videoWidth === 0)
      return { text: '', contrast: 0, width: 0, height: 0 };
    width = video.videoWidth;
    height = video.videoHeight;
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const context = canvas.getContext('2d', { willReadFrequently: true });
    context.drawImage(video, 0, 0);
    pixels = context.getImageData(0, 0, width, height).data;
  }
  let decoded = '',
    minimum = 255,
    maximum = 0;
  for (let character = 0; character < text.length; character++) {
    let bits = '';
    for (let row = 0; row < 7; row++)
      for (let column = 0; column < 5; column++) {
        const x = Math.min(
          width - 1,
          Math.floor(((144 + character * 144 + (column + 0.5) * 24) * width) / 1920),
        );
        const y = Math.min(height - 1, Math.floor(((144 + (row + 0.5) * 24) * height) / 1080));
        const offset = (y * width + x) * 4;
        const value = (pixels[offset] + pixels[offset + 1] + pixels[offset + 2]) / 3;
        minimum = Math.min(minimum, value);
        maximum = Math.max(maximum, value);
        bits += value < 128 ? '1' : '0';
      }
    let best = '?',
      distance = 36;
    for (const [glyph, rows] of Object.entries(glyphs)) {
      const pattern = rows.join('');
      let differences = 0;
      for (let index = 0; index < bits.length; index++)
        if (bits[index] !== pattern[index]) differences++;
      if (differences < distance) {
        best = glyph;
        distance = differences;
      }
    }
    decoded += distance <= 2 ? best : '?';
  }
  const contrast = Math.round(maximum - minimum);
  return { text: contrast >= 150 ? decoded : '', contrast, width, height };
}

module.exports = { text, glyphs, installPresentationCapture, decodePresentationFrame };
