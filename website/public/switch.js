// Pick the install panel matching this machine, and let the dropdown override
// it. Adding `.js` to <html> is what switches the CSS from "show every panel"
// to "show the active one", so with JavaScript off the page degrades to all
// three panels under their own headings: worse, but not broken.
//
// This lives in public/ rather than in the page so it is served as its own
// file, which keeps the Content-Security-Policy at `script-src 'self'` with no
// inline-script exception.
(function () {
  var root = document.documentElement;
  root.classList.add('js');

  var select = document.querySelector('#os');
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
  }

  var detected = detect();
  show(detected);
  if (select) {
    select.value = detected;
    select.addEventListener('change', function () {
      show(select.value);
    });
  }
})();
