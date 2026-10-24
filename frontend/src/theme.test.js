import { describe, it, expect, beforeEach, afterAll } from 'bun:test'

function makeStorage() {
  const store = new Map()
  return {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
    clear: () => store.clear(),
  }
}

const originalWindow = globalThis.window
const mockStorage = makeStorage()
globalThis.window = {
  localStorage: mockStorage,
  matchMedia: (query) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }),
  addEventListener: () => {},
  removeEventListener: () => {},
}

const {
  THEME_KEY,
  parseThemePreference,
  resolveTheme,
  getStoredThemePreference,
  setThemePreference,
  subscribeTheme,
  applyTheme,
} = await import('./theme.js')

describe('theme preference and resolution', () => {
  beforeEach(() => {
    mockStorage.clear()
  })

  afterAll(() => {
    globalThis.window = originalWindow
  })

  it('parses valid and invalid preference strings', () => {
    expect(parseThemePreference('light')).toBe('light')
    expect(parseThemePreference('dark')).toBe('dark')
    expect(parseThemePreference('system')).toBe('system')
    expect(parseThemePreference('auto')).toBe('system')
    expect(parseThemePreference('')).toBe('system')
    expect(parseThemePreference(null)).toBe('system')
    expect(parseThemePreference(undefined)).toBe('system')
    expect(parseThemePreference('DARK')).toBe('system')
  })

  it('resolves explicit preference over system state', () => {
    expect(resolveTheme('light', true)).toBe('light')
    expect(resolveTheme('light', false)).toBe('light')
    expect(resolveTheme('dark', true)).toBe('dark')
    expect(resolveTheme('dark', false)).toBe('dark')
  })

  it('resolves system preference according to system state', () => {
    expect(resolveTheme('system', true)).toBe('dark')
    expect(resolveTheme('system', false)).toBe('light')
    expect(resolveTheme('unknown', true)).toBe('dark')
    expect(resolveTheme('unknown', false)).toBe('light')
  })

  it('stores explicit preference and removes item for system', () => {
    setThemePreference('dark')
    expect(mockStorage.getItem(THEME_KEY)).toBe('dark')
    expect(getStoredThemePreference()).toBe('dark')

    setThemePreference('light')
    expect(mockStorage.getItem(THEME_KEY)).toBe('light')
    expect(getStoredThemePreference()).toBe('light')

    setThemePreference('system')
    expect(mockStorage.getItem(THEME_KEY)).toBeNull()
    expect(getStoredThemePreference()).toBe('system')
  })

  it('notifies subscribers when preference changes', () => {
    let callCount = 0
    const unsubscribe = subscribeTheme(() => {
      callCount += 1
    })

    setThemePreference('dark')
    expect(callCount).toBe(1)

    setThemePreference('light')
    expect(callCount).toBe(2)

    unsubscribe()
    setThemePreference('system')
    expect(callCount).toBe(2)
  })

  it('applies theme to document element dataset when document exists', () => {
    const originalDoc = globalThis.document
    const htmlEl = { dataset: {} }
    globalThis.document = { documentElement: htmlEl }

    try {
      applyTheme('dark')
      expect(htmlEl.dataset.theme).toBe('dark')
      applyTheme('light')
      expect(htmlEl.dataset.theme).toBe('light')
    } finally {
      globalThis.document = originalDoc
    }
  })
})
