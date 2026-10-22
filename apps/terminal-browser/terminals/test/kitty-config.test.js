const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const { test } = require('node:test');
const { enableKittyGraphics } = require('../dist/terminals/herdr.js');

const cases = [
  ['inline comment', '[experimental]\nkitty_graphics = false # disabled\n', '[experimental]\nkitty_graphics = true # disabled\n'],
  ['spacing and header comment', ' [experimental] # features\n  kitty_graphics  =  false  # disabled\n', ' [experimental] # features\n  kitty_graphics  =  true  # disabled\n'],
  ['already true with comment', '[experimental]\nkitty_graphics = true # enabled\n', '[experimental]\nkitty_graphics = true # enabled\n'],
  ['no section', 'onboarding = false\n', 'onboarding = false\n\n[experimental]\nkitty_graphics = true\n'],
  ['other section with same key', '[other]\nkitty_graphics = false\n\n[experimental]\nallow_nested = true\n', '[other]\nkitty_graphics = false\n\n[experimental]\nkitty_graphics = true\nallow_nested = true\n'],
  ['true in other section', '[other]\nkitty_graphics = true\n', '[other]\nkitty_graphics = true\n\n[experimental]\nkitty_graphics = true\n'],
];
for (const [name, input, expected] of cases) test(`kitty config: ${name} remains valid TOML`, () => {
  const output = enableKittyGraphics(input);
  assert.equal(output, expected);
  const parsed = JSON.parse(execFileSync('python3', ['-c', 'import sys,tomllib,json; print(json.dumps(tomllib.loads(sys.stdin.read())))'], { input: output, encoding: 'utf8' }));
  assert.equal(parsed.experimental.kitty_graphics, true);
  assert.equal(enableKittyGraphics(output), output);
});
