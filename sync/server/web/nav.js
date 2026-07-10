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
(function () {
  if (document.getElementById('rm-site-nav')) return;

  var SECTIONS = [
    ['notes', '/notes/'],
    ['updates', '/updates/'],
    ['notebook', '/notebook/'],
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
    '@keyframes rm-pulse{50%{opacity:.35}}';

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
