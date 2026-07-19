// Site-wide navigation for remarkable.exe.xyz.
//
// Injected into every HTML response by nginx (sub_filter '</body>' — see
// server/nginx/default.conf), so the independently-generated sections
// (Shelley's /notes/, the digest agent's /updates/, the /notebook/ SPA,
// even bare autoindex listings) share one nav without any generator
// having to know about it.
//
// Every page gets the same slim static bar prepended to <body>. Static,
// not fixed: app pages like /notebook/ have their own sticky chrome that
// should win once you scroll.
//
// Theme contract: the site follows the OS theme (prefers-color-scheme) by
// default; an explicit pick via the nav toggle persists in localStorage
// "rm-theme" and wins from then on (double-click the toggle to go back to
// following the system). The effective theme is mirrored onto
// <html data-theme="light|dark">. nginx injects
// a tiny bootstrap script into <head> so the attribute is set before first
// paint (no flash); this file reuses the bootstrap's window.__rmTheme (or
// re-creates it), owns the toggle button at the top right, and themes its
// own bar. Pages opt into light mode with `html[data-theme=light] { ... }`.
(function () {
  if (document.getElementById('rm-site-nav')) return;

  var THEME_KEY = 'rm-theme';

  // Shared theme resolver (normally installed by the <head> bootstrap).
  var theme = window.__rmTheme || (function () {
    function stored() {
      try {
        var t = localStorage.getItem(THEME_KEY);
        return (t === 'light' || t === 'dark') ? t : null;
      } catch (e) { return null; }
    }
    function sys() {
      try { return matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'; }
      catch (e) { return 'dark'; }
    }
    function apply() {
      var t = stored() || sys(), d = document.documentElement;
      d.setAttribute('data-theme', t);
      d.style.colorScheme = t;
    }
    apply();
    try {
      var mq = matchMedia('(prefers-color-scheme: light)');
      var fn = function () { if (!stored()) apply(); };
      mq.addEventListener ? mq.addEventListener('change', fn) : mq.addListener(fn);
    } catch (e) {}
    return { stored: stored, sys: sys, apply: apply };
  })();

  function currentTheme() {
    var t = document.documentElement.getAttribute('data-theme');
    return (t === 'light' || t === 'dark') ? t : 'dark';
  }

  function setTheme(t) {
    try { localStorage.setItem(THEME_KEY, t); } catch (e) {}
    theme.apply();
    updateToggle();
  }

  function resetTheme() {
    try { localStorage.removeItem(THEME_KEY); } catch (e) {}
    theme.apply();
    updateToggle();
  }

  var SECTIONS = [
    ['notes', '/notes/'],
    ['updates', '/updates/'],
    ['notebook', '/notebook/'],
    ['papier', '/papier/'],
  ];
  var path = location.pathname;

  function isCurrent(href) {
    return path === href || path.indexOf(href) === 0;
  }

  function makeLinks(cls) {
    var frag = document.createDocumentFragment();
    SECTIONS.forEach(function (s) {
      var a = document.createElement('a');
      a.textContent = s[0];
      a.href = s[1];
      a.className = cls + (isCurrent(s[1]) ? ' current' : '');
      if (s[0] === 'notebook') {
        // The dot marks notebook as the live one (vs the daily notes).
        // It pulses green when the tablet's stroke stream is actually
        // connected — same semantics as the notebook page's own indicator.
        var dot = document.createElement('span');
        dot.className = 'rm-live-dot';
        dot.title = 'live notebook';
        a.appendChild(dot);
      }
      frag.appendChild(a);
    });
    return frag;
  }

  var SUN = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>';
  var MOON = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/></svg>';

  var toggleBtn;

  function updateToggle() {
    if (!toggleBtn) return;
    var light = currentTheme() === 'light';
    toggleBtn.innerHTML = light ? MOON : SUN;
    var label = 'Switch to ' + (light ? 'dark' : 'light') + ' mode';
    label += theme.stored() ? ' (double-click to follow system)' : ' (following system)';
    toggleBtn.title = label;
    toggleBtn.setAttribute('aria-label', label);
    toggleBtn.setAttribute('aria-pressed', light ? 'true' : 'false');
  }

  var css =
    '#rm-site-nav{display:flex;align-items:center;gap:18px;padding:8px 16px;' +
    'background:rgb(15,17,21);border-bottom:1px solid rgba(107,114,128,.25);' +
    "font-family:'Google Sans Code',ui-monospace,'SF Mono',Menlo,Consolas,monospace;font-size:13px;line-height:1}" +
    '#rm-site-nav .rm-brand{color:rgb(229,231,235);text-decoration:none;font-weight:500;margin-right:6px}' +
    '#rm-site-nav .rm-brand:hover{color:rgb(245,158,11)}' +
    '.rm-nav-link{color:rgb(107,114,128);text-decoration:none;padding:2px 2px}' +
    '.rm-nav-link:hover{color:rgb(229,231,235)}' +
    '.rm-nav-link.current{color:rgb(245,158,11)}' +
    '.rm-live-dot{display:inline-block;width:7px;height:7px;border-radius:50%;' +
    'background:#555;margin-left:6px;vertical-align:1px}' +
    '.rm-live-dot.on{background:#3fa34d;animation:rm-pulse 2.4s ease-in-out infinite}' +
    '@keyframes rm-pulse{50%{opacity:.35}}' +
    '.rm-theme-toggle{margin-left:auto;display:inline-flex;align-items:center;justify-content:center;' +
    'width:26px;height:26px;padding:0;border-radius:7px;cursor:pointer;' +
    'background:transparent;border:1px solid rgba(107,114,128,.35);color:rgb(229,231,235)}' +
    '.rm-theme-toggle:hover{border-color:rgb(245,158,11);color:rgb(245,158,11)}' +
    // Light theme (toggle persists <html data-theme="light">)
    'html[data-theme=light] #rm-site-nav{background:rgb(250,250,249);border-bottom-color:rgba(20,24,32,.12)}' +
    'html[data-theme=light] #rm-site-nav .rm-brand{color:rgb(24,25,28)}' +
    'html[data-theme=light] #rm-site-nav .rm-brand:hover{color:rgb(217,119,6)}' +
    'html[data-theme=light] .rm-nav-link{color:rgb(110,115,123)}' +
    'html[data-theme=light] .rm-nav-link:hover{color:rgb(24,25,28)}' +
    'html[data-theme=light] .rm-nav-link.current{color:rgb(217,119,6)}' +
    'html[data-theme=light] .rm-theme-toggle{border-color:rgba(20,24,32,.18);color:rgb(64,68,74)}' +
    'html[data-theme=light] .rm-theme-toggle:hover{border-color:rgb(217,119,6);color:rgb(217,119,6)}';

  var style = document.createElement('style');
  style.textContent = css;
  document.head.appendChild(style);

  var bar = document.createElement('div');
  bar.id = 'rm-site-nav';
  var brand = document.createElement('a');
  brand.textContent = 'remarkable';
  brand.href = '/notes/';
  brand.className = 'rm-brand';
  bar.appendChild(brand);
  bar.appendChild(makeLinks('rm-nav-link'));

  toggleBtn = document.createElement('button');
  toggleBtn.className = 'rm-theme-toggle';
  toggleBtn.type = 'button';
  toggleBtn.addEventListener('click', function () {
    setTheme(currentTheme() === 'light' ? 'dark' : 'light');
  });
  toggleBtn.addEventListener('dblclick', function () {
    resetTheme();
  });
  bar.appendChild(toggleBtn);
  updateToggle();

  document.body.insertBefore(bar, document.body.firstChild);

  // Light the dot when the tablet's stroke stream is connected right now
  // (one-shot check via the relay's health endpoint; stays dim on failure).
  fetch('/notebook/live/health', { cache: 'no-store' })
    .then(function (r) { return r.ok ? r.json() : null; })
    .then(function (s) {
      if (!s || !s.live) return;
      document.querySelectorAll('.rm-live-dot').forEach(function (d) {
        d.classList.add('on');
        d.title = 'tablet is writing live now';
      });
    })
    .catch(function () {});
})();
