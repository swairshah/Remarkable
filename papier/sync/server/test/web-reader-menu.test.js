'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');

const htmlPath = path.resolve(__dirname, '../web/index.html');
const navPath = path.resolve(__dirname, '../../../../sync/server/web/nav.js');

test('reader actions stay in the sticky header and browser Back returns home', () => {
  const html = fs.readFileSync(htmlPath, 'utf8');
  const script = html.match(/<script>([\s\S]*?)<\/script>/);

  assert.ok(script, 'viewer has an inline application script');
  assert.doesNotThrow(() => new Function(script[1]), 'viewer script parses');
  assert.match(html, /id="reader-menu-btn"[^>]*aria-haspopup="menu"/);
  assert.match(html, /body\.reading \.reader-actions \{ display:block; \}/);
  assert.match(html, /\.reader-menu-toggle \{[^}]*min-width:44px; min-height:44px;/);
  assert.match(html, /readerMenuItem\('▦', 'Adjust margins'/);
  assert.match(html, /readerMenuItem\('⤡', 'PDF viewer'/);
  assert.match(html, /readerMenuItem\('⇪', 'Send to Kindle'/);
  assert.match(html, /readerMenuItem\('⌂', 'Home', goFiles\)/);
  assert.match(html, /sharedToggle && sharedToggle\.innerHTML/);
  assert.match(html, /menu\.querySelector\('\[data-reader-action="theme"\]'\)/);
  assert.match(html, /if \(theme\) syncReaderThemeAction\(theme\)/);
  const nav = fs.readFileSync(navPath, 'utf8');
  for (const icon of ['SUN', 'MOON']) {
    const shared = nav.match(new RegExp("var " + icon + " = '([^']+)';"));
    const reader = html.match(new RegExp("const READER_THEME_" + icon + " = '([^']+)';"));
    assert.ok(shared && reader, icon + ' SVG is defined in both theme controls');
    assert.equal(reader[1], shared[1], icon + ' SVG exactly matches the top-right switch');
  }
  assert.match(html, /history\.pushState\(\{ papier:'doc', id:d\.id \}/);
  assert.match(html, /window\.addEventListener\('popstate'/);
  assert.match(html, /goFiles\(\{ fromHistory:true \}\)/);
  assert.doesNotMatch(html, /bar\.className = 'docbar'/, 'reader no longer renders a scrolling action strip');
});
