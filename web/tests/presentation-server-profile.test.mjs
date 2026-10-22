import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

// The owned test server's environment is built by the Rust harness
// (xtask/src/test_env/server.rs) from nothing but PATH and the opt-ins; these
// checks pin the presentation profile's fixed log filter in that source.
const source = readFileSync(new URL('../../xtask/src/test_env/server.rs', import.meta.url), 'utf8');

function constant(name) {
  const match = source.match(
    new RegExp(`const ${name}: &str = "((?:[^"\\\\]|\\\\.|\\\\\\n)*)";`, 's'),
  );
  assert.ok(match, `${name} is declared as one string constant`);
  // A `\` at the end of a line continues the literal without the indentation.
  return match[1].replace(/\\\n\s*/g, '');
}

test('owned presentation diagnostics use only the fixed bandwidth log profile', () => {
  assert.equal(
    constant('PRESENTATION_LOG_FILTER'),
    'simplestchat=info,mediasoup=warn,sqlx::pool=warn,simplestchat::signaling::connection=debug,simplestchat::media::transport_manager=debug',
  );
  const block = source.match(/if self\.plan\.presentation \{[\s\S]*?\n {8}\}/)?.[0];
  assert.ok(block, 'the profile is an isolated explicit opt-in block');
  assert.match(block, /"RUST_LOG"\.into\(\), PRESENTATION_LOG_FILTER\.into\(\)/);
  assert.equal(
    source.match(/"RUST_LOG"/g).length,
    1,
    'no other code path sets the server log filter',
  );
  assert.doesNotMatch(
    source,
    /env\("RUST_LOG"\)|var\("RUST_LOG"\)/,
    'an operator filter is never read',
  );
});

test('the server starts from an emptied environment with the fixed opt-in profile', () => {
  assert.match(
    source,
    /\.current_dir\(&self\.plan\.workdir\)\n\s+\.env_clear\(\)\n\s+\.envs\(server_env\)/,
  );
  assert.match(source, /presentation: flag\(environment, "PRESENTATION_E2E"\)/);
  assert.match(
    source,
    /fn flag\(environment: &Environment, name: &str\) -> bool \{\n\s+env\(environment, name\)\.is_some_and\(\|value\| value == "1"\)/,
  );
  assert.match(
    source,
    /fn env\(environment: &Environment, name: &str\) -> Option<String> \{\n\s+environment\n\s+\.get\(OsStr::new\(name\)\)/,
  );
});
