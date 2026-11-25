// Two small jobs: show the install panel matching this machine (with the tabs
// as an override), and copy the install command.
//
// Adding `.js` to <html> is what switches the CSS from "show every panel" to
// "show the active one", so with JavaScript off the page degrades to all three
// panels under their own headings: worse, but not broken.
//
// This lives in public/ rather than in the page so it is served as its own
// file, which keeps the Content-Security-Policy at `script-src 'self'` with no
// inline-script exception.
(function () {
  document.documentElement.classList.add('js');

  var tabs = Array.prototype.slice.call(document.querySelectorAll('[role="tab"]'));
  var panels = Array.prototype.slice.call(document.querySelectorAll('.panel'));

  function detect() {
    var data = navigator.userAgentData;
    var platform = (data && data.platform) || navigator.platform || '';
    var haystack = platform + ' ' + navigator.userAgent;
    if (/Win/i.test(haystack)) return 'windows';
    // Mac before Linux: iPadOS reports a Mac platform, and either way the macOS
    // panel is the closer answer.
    if (/Mac|iPhone|iPad|iPod/i.test(haystack)) return 'macos';
    return 'linux';
  }

  function show(os) {
    panels.forEach(function (panel) {
      if (panel.getAttribute('data-os') === os) panel.setAttribute('data-active', '');
      else panel.removeAttribute('data-active');
    });
    tabs.forEach(function (tab) {
      tab.setAttribute('aria-selected', String(tab.getAttribute('data-tab') === os));
    });
  }

  show(detect());

  tabs.forEach(function (tab, index) {
    tab.addEventListener('click', function () {
      show(tab.getAttribute('data-tab'));
    });
    // Arrow keys move between tabs, which is what a tablist is expected to do.
    tab.addEventListener('keydown', function (event) {
      var step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
      if (!step) return;
      event.preventDefault();
      var next = tabs[(index + step + tabs.length) % tabs.length];
      next.focus();
      show(next.getAttribute('data-tab'));
    });
  });

  document.querySelectorAll('.copy').forEach(function (button) {
    button.addEventListener('click', function () {
      var source = document.getElementById(button.getAttribute('data-copy'));
      if (!source || !navigator.clipboard) return;
      navigator.clipboard.writeText(source.textContent.trim()).then(function () {
        var previous = button.textContent;
        button.textContent = 'Copied';
        setTimeout(function () {
          button.textContent = previous;
        }, 1600);
      });
    });
  });
})();
