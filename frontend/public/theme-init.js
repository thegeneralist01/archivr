(function () {
  try {
    var stored = localStorage.getItem('archivr-theme');
    var isDark = stored === 'dark' || (stored !== 'light' && window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches);
    document.documentElement.dataset.theme = isDark ? 'dark' : 'light';
  } catch (e) {
    /* ignore storage access error */
  }
})();
