const assert = require('node:assert/strict');
const { test } = require('node:test');
const { currentMultiplexer, multiplexerBin, detect } = require('../dist/index.js');

for (const runtime of ['herdr', 'ikesu', 'gtx']) {
  test(`${runtime} selects its existing pane namespace and preferred executable`, () => {
    const env = { [`${runtime.toUpperCase()}_PANE_ID`]: 'p1' };
    assert.equal(currentMultiplexer(env), runtime);
    assert.equal(detect(env).name, runtime);
    assert.equal(multiplexerBin(runtime, env), runtime);
    env[`${runtime.toUpperCase()}_BIN_PATH`] = `/installed/${runtime}`;
    assert.equal(multiplexerBin(runtime, env), `/installed/${runtime}`);
  });
}
test('unknown runtime has no multiplexer', () => assert.equal(currentMultiplexer({}), null));
