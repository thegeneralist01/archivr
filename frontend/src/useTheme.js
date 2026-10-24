import { useSyncExternalStore, useCallback } from 'react'
import {
  getStoredThemePreference,
  systemPrefersDark,
  resolveTheme,
  setThemePreference,
  subscribeTheme,
  getThemeSnapshot,
} from './theme.js'

export function useTheme() {
  const snapshot = useSyncExternalStore(subscribeTheme, getThemeSnapshot, () => 'system:light')
  const [preference, resolved] = snapshot.split(':')

  const setPreference = useCallback((next) => {
    setThemePreference(next)
  }, [])

  const toggleTheme = useCallback(() => {
    // If currently resolving to dark -> switch to light. Otherwise -> switch to dark.
    const currentResolved = resolveTheme(getStoredThemePreference(), systemPrefersDark())
    const next = currentResolved === 'dark' ? 'light' : 'dark'
    setThemePreference(next)
  }, [])

  return {
    preference,
    resolved,
    setPreference,
    toggleTheme,
  }
}
