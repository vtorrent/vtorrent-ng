import { useCallback, useEffect, useState } from 'react'

export type Theme = 'modern' | 'legacy'

const STORAGE_KEY = 'vtr-theme'

function initialTheme(): Theme {
  try {
    return localStorage.getItem(STORAGE_KEY) === 'modern' ? 'modern' : 'legacy'
  } catch {
    return 'legacy'
  }
}

/// Global UI theme (modern teal vs legacy espresso). Persists to localStorage
/// and flips `data-theme` on <html>, which index.css uses for the palette.
export function useTheme() {
  const [theme, setTheme] = useState<Theme>(initialTheme)

  useEffect(() => {
    document.documentElement.dataset.theme = theme === 'legacy' ? 'legacy' : 'modern'
    try {
      localStorage.setItem(STORAGE_KEY, theme)
    } catch {
      // Private mode: theme still applies for this session.
    }
  }, [theme])

  const toggle = useCallback(
    () => setTheme(t => (t === 'legacy' ? 'modern' : 'legacy')),
    [],
  )

  return { theme, toggle }
}
