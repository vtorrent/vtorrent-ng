try {
  if (localStorage.getItem('vtr-theme') !== 'modern') {
    document.documentElement.dataset.theme = 'legacy'
  }
} catch {
  // Storage can be unavailable in hardened webviews; keep the default theme.
}
