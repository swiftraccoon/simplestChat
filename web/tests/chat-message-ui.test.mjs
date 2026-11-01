import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { uiFixture } from './ui-fixture.mjs';

/** Loads the renderer chat rows use; each call renders text into a new paragraph. */
async function chatText() {
  const f = await uiFixture();
  f.document.createTextNode = (text) => f.ui.el('text', text);
  const { appendLinkedText } = await loadTypeScript('src/chat-message-ui.ts', {
    modules: { './ui': f.ui },
    globals: { document: f.document },
  });
  return (content, mention) => {
    const parent = f.ui.el('p');
    appendLinkedText(parent, content, mention);
    return parent;
  };
}

// Cyrillic а, р, р, ӏ and е: the hostname reads as apple.com.
const homograph = 'https://\u0430\u0440\u0440\u04CF\u0435.com/x';

test('a link shows a host name in punycode', async () => {
  const render = await chatText();
  const text = render(`see ${homograph} now`);
  const link = text.querySelector('a');
  assert.ok(link.textContent.startsWith('https://xn--'), link.textContent);
  assert.equal(link.textContent, 'https://xn--80ak6aa92e.com/x');
  assert.equal(link.href, homograph);
  assert.equal(link.target, '_blank');
  assert.equal(link.rel, 'noopener noreferrer');
  assert.equal(text.textContent, 'see https://xn--80ak6aa92e.com/x now');
});

test('an address the URL parser rejects stays plain text', async () => {
  const render = await chatText();
  // Both match the link pattern; the port is out of range and the IPv6 host never closes.
  for (const raw of ['https://host:99999/x', 'https://[']) {
    const content = `see ${raw} now`;
    const text = render(content);
    assert.equal(text.querySelector('a'), null, raw);
    assert.equal(text.textContent, content);
  }
  // The text stays in the plain-text run, so a mention inside it is still marked.
  const text = render('see https://host:99999/@local', '@local');
  assert.deepEqual(
    text.querySelectorAll('mark').map((node) => node.textContent),
    ['@local'],
  );
});

test('a bare host reads with a trailing slash', async () => {
  const render = await chatText();
  const link = render('https://example.com').querySelector('a');
  assert.equal(link.textContent, 'https://example.com/');
  assert.equal(link.href, 'https://example.com');
});

test('an address with user information before its host stays plain text', async () => {
  const render = await chatText();
  // What comes before the @ can make the host after it read as another.
  for (const raw of ['https://good.example@evil.example/', 'https://:secret@evil.example/']) {
    const content = `see ${raw} now`;
    const text = render(content);
    assert.equal(text.querySelector('a'), null, raw);
    assert.equal(text.textContent, content);
  }
  // An empty user information part hides nothing.
  const link = render('https://@evil.example').querySelector('a');
  assert.equal(link.textContent, 'https://evil.example/');
  assert.equal(link.href, 'https://@evil.example');
});

test('punctuation that ends a sentence stays out of the link', async () => {
  const render = await chatText();
  for (const [content, shown] of [
    ['see https://example.com.', 'see https://example.com/.'],
    ['https://example.com:', 'https://example.com/:'],
  ]) {
    const text = render(content);
    const link = text.querySelector('a');
    assert.equal(link.textContent, 'https://example.com/', content);
    assert.equal(link.href, 'https://example.com');
    assert.equal(link.nextSibling.textContent, content.at(-1));
    assert.equal(text.textContent, shown);
  }
});

test('a closing parenthesis stays out of the link unless the address opens one', async () => {
  const render = await chatText();
  const foo = 'https://en.wikipedia.org/wiki/Foo_(bar)';
  const mercury = 'https://en.wikipedia.org/wiki/Mercury_(planet)';
  // Message, href, link text, then the text after the link.
  for (const [content, href, shown, after] of [
    ['(https://example.com)', 'https://example.com', 'https://example.com/', ')'],
    ['(see https://example.com).', 'https://example.com', 'https://example.com/', ').'],
    [foo, foo, foo, ''],
    [`${foo}.`, foo, foo, '.'],
    [`(see ${mercury})`, mercury, mercury, ')'],
  ]) {
    const link = render(content).querySelector('a');
    assert.equal(link.href, href, content);
    assert.equal(link.textContent, shown, content);
    assert.equal(link.nextSibling.textContent, after, content);
  }
});
