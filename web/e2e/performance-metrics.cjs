function decodedVideoFrames(before, after) {
  const previous = new Map(before.filter(item => item.kind === 'video').map(item => [item.id, item.framesDecoded || 0]));
  return after.filter(item => item.kind === 'video').reduce((sum, item) => {
    return sum + Math.max(0, (item.framesDecoded || 0) - (previous.get(item.id) || 0));
  }, 0);
}
module.exports = { decodedVideoFrames };
