'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');

const htmlPath = path.resolve(__dirname, '../web/index.html');

test('document cards expose the recoverable delete API from a context menu', () => {
  const html = fs.readFileSync(htmlPath, 'utf8');
  const script = html.match(/<script>([\s\S]*?)<\/script>/);

  assert.ok(script, 'viewer has an inline application script');
  assert.doesNotThrow(() => new Function(script[1]), 'viewer script parses');
  assert.match(html, /card\.oncontextmenu\s*=\s*\(e\)\s*=>\s*openDocMenu\(e, d, card\)/);
  assert.match(html, /data-action="delete"/);
  assert.match(html, /fetch\(API \+ 'delete\?id=' \+ encodeURIComponent\(d\.id\), \{ method:'POST' \}\)/);
  assert.match(html, /state\.docs = state\.docs\.filter\(doc => doc\.id !== d\.id\)/);
  assert.doesNotMatch(html, /folderGlyph\(\)[\s\S]{0,800}oncontextmenu/, 'folder cards do not get document actions');
});
