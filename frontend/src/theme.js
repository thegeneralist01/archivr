export const THEME_KEY = 'archivr-theme'
export const THEMES = ['system', 'light', 'dark']

export function parseThemePreference(val) {
  return val === 'light' || val === 'dark' ? val : 'system'
}

export function getStoredThemePreference() {
  if (typeof window === 'undefined' || !window.localStorage) return 'system'
  try {
    return parseThemePreference(window.localStorage.getItem(THEME_KEY))
  } catch {
    return 'system'
  }
}

export function systemPrefersDark() {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return false
  try {
    return window.matchMedia('(prefers-color-scheme: dark)').matches
  } catch {
    return false
  }
}

export function resolveTheme(preference, isDark) {
  const pref = parseThemePreference(preference)
  if (pref === 'system') return isDark ? 'dark' : 'light'
  return pref
}

export function applyTheme(preference) {
  if (typeof document === 'undefined') return 'light'
  const isDark = systemPrefersDark()
  const resolved = resolveTheme(preference, isDark)
  document.documentElement.dataset.theme = resolved
  return resolved
}

const listeners = new Set()

function notify() {
  for (const cb of listeners) {
    try { cb() } catch { /* ignore subscriber error */ }
  }
}

export function setThemePreference(preference) {
  const valid = parseThemePreference(preference)
  if (typeof window !== 'undefined' && window.localStorage) {
    try {
      if (valid === 'system') {
        window.localStorage.removeItem(THEME_KEY)
      } else {
        window.localStorage.setItem(THEME_KEY, valid)
      }
    } catch {
      // storage unavailable / quota exceeded
    }
  }
  applyTheme(valid)
  notify()
  return valid
}

// Attach listeners once in browser environments
let mediaQueryList = null
if (typeof window !== 'undefined' && typeof window.matchMedia === 'function') {
  try {
    mediaQueryList = window.matchMedia('(prefers-color-scheme: dark)')
    const onMediaChange = () => {
      const currentPref = getStoredThemePreference()
      if (currentPref === 'system') {
        applyTheme('system')
        notify()
      }
    }
    if (typeof mediaQueryList.addEventListener === 'function') {
      mediaQueryList.addEventListener('change', onMediaChange)
    } else if (typeof mediaQueryList.addListener === 'function') {
      mediaQueryList.addListener(onMediaChange)
    }
  } catch {
    // ignore matchMedia setup failures
  }

  window.addEventListener('storage', (e) => {
    if (e.key === THEME_KEY) {
      applyTheme(getStoredThemePreference())
      notify()
    }
  })
}

export function subscribeTheme(callback) {
  listeners.add(callback)
  return () => {
    listeners.delete(callback)
  }
}

export function getThemeSnapshot() {
  const preference = getStoredThemePreference()
  const resolved = resolveTheme(preference, systemPrefersDark())
  return `${preference}:${resolved}`
}
if (typeof document !== 'undefined') {
  try {
    applyTheme(getStoredThemePreference())
  } catch {
    /* ignore in environments with partial DOM mocks */
  }
}
