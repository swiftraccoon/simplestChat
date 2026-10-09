/** Composer/preview seams for messaging integration; attachments.test exercises native APIs. */
export function attachmentFixture() {
  const composers = [],
    renders = [];
  class AttachmentComposer {
    files = [];
    resets = 0;
    consumed = 0;
    disposed = false;
    blocked = false;
    constructor(options) {
      this.options = options;
      composers.push(this);
    }
    get hasFiles() {
      return this.files.length > 0;
    }
    ready() {
      if (this.blocked) throw new Error('Wait for uploads');
      return [...this.files];
    }
    consume() {
      this.consumed++;
      this.files = [];
    }
    reset() {
      this.resets++;
      this.files = [];
    }
    dispose() {
      this.reset();
      this.disposed = true;
    }
  }
  function renderAttachments(host, files, options) {
    const render = { host, files, options, disposed: false };
    renders.push(render);
    return () => {
      render.disposed = true;
    };
  }
  return { module: { AttachmentComposer, renderAttachments }, composers, renders };
}
