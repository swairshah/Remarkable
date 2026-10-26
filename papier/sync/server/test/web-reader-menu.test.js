'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');

const htmlPath = path.resolve(__dirname, '../web/index.html');
const navPath = path.resolve(__dirname, '../../../../sync/server/web/nav.js');

test('home actions are compact while reader actions stay sticky and Back returns home', () => {
  const html = fs.readFileSync(htmlPath, 'utf8');
  const script = html.match(/<script>([\s\S]*?)<\/script>/);

  assert.ok(script, 'viewer has an inline application script');
  assert.doesNotThrow(() => new Function(script[1]), 'viewer script parses');
  assert.match(html, /id="reader-menu-btn"[^>]*aria-haspopup="menu"/);
  assert.match(html, /body\.reading \.reader-actions \{ display:block; \}/);
  assert.match(html, /body:not\(\.reading\) header \.brand, body:not\(\.reading\) header \.badge \{ display:none; \}/);
  assert.match(html, /body:not\(\.reading\) header \{\s*grid-template-columns:44px minmax\(0,1fr\) minmax\(0,1fr\);/);
  assert.match(html, /body:not\(\.reading\) header #compose-btn \{ grid-column:2; grid-row:1; \}/);
  assert.match(html, /body:not\(\.reading\) header #add \{ grid-column:3; grid-row:1; \}/);
  assert.match(html, /id="home-view-btn"[^>]*aria-haspopup="menu"/);
  assert.match(html, /data-home-view="icons"[^>]*>[\s\S]*?Small icons/);
  assert.match(html, /data-home-view="list"[^>]*>[\s\S]*?List/);
  assert.match(html, /\.grid\.view-icons \{ grid-template-columns:repeat\(auto-fill, minmax\(112px, 1fr\)\);/);
  assert.match(html, /\.grid\.view-list \{ display:flex; flex-direction:column;/);
  assert.match(html, /const HOME_VIEW_KEY = 'papier-home-view'/);
  assert.match(html, /localStorage\.setItem\(HOME_VIEW_KEY, view\)/);
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
