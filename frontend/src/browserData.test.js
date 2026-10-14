import { describe, test, expect } from 'bun:test'
import { toRows, rowsToMap, validateBrowserData, buildBrowserSessionPayload } from './browserData.js'

describe('toRows', () => {
  test('maps name field for cookies', () => {
    const rows = toRows([{ name: 'session', value: 'abc', httpOnly: true }], 'name')
    expect(rows).toHaveLength(1)
    expect(rows[0].name).toBe('session')
    expect(rows[0].value).toBe('abc')
    expect(rows[0].httpOnly).toBe(true)
    expect(rows[0].included).toBe(true)
    expect(typeof rows[0].id).toBe('number')
  })

  test('maps key field for localStorage', () => {
    const rows = toRows([{ key: 'pref', value: '1' }], 'key')
    expect(rows[0].name).toBe('pref')
    expect(rows[0].value).toBe('1')
    expect(rows[0].httpOnly).toBeUndefined()
  })
})

describe('rowsToMap', () => {
  test('includes only included rows with non-empty names', () => {
    const rows = [
      { id: 1, name: 'a', value: 'x', included: true },
      { id: 2, name: 'b', value: 'y', included: false },
      { id: 3, name: '', value: 'z', included: true },
    ]
    const map = rowsToMap(rows)
    expect(Object.keys(map)).toEqual(['a'])
    expect(map.a).toBe('x')
  })

  test('preserves a localStorage key named __proto__', () => {
    const map = rowsToMap([{ id: 1, name: '__proto__', value: 'saved', included: true }])
    expect(Object.keys(map)).toEqual(['__proto__'])
    expect(map.__proto__).toBe('saved')
  })
})

describe('validateBrowserData', () => {
  function make(overrides = {}) {
    return {
      useCookies: true,
      includeStorage: true,
      cookieRows: [],
      storageRows: [],
      ...overrides,
    }
  }

  test('returns null for valid empty data', () => {
    expect(validateBrowserData(make())).toBeNull()
  })

  test('rejects a duplicate included cookie name', () => {
    const cookieRows = [
      { id: 1, name: 'dup', value: 'a', included: true },
      { id: 2, name: 'dup', value: 'b', included: true },
    ]
    const err = validateBrowserData(make({ cookieRows }))
    expect(err).toMatch(/Duplicate cookie name/)
  })

  test('ignores duplicate excluded cookie names', () => {
    const cookieRows = [
      { id: 1, name: 'dup', value: 'a', included: false },
      { id: 2, name: 'dup', value: 'b', included: false },
    ]
    expect(validateBrowserData(make({ cookieRows }))).toBeNull()
  })

  test('rejects semicolon in cookie value', () => {
    const cookieRows = [{ id: 1, name: 'k', value: 'a;b', included: true }]
    const err = validateBrowserData(make({ cookieRows }))
    expect(err).toBeTruthy()
    expect(err).toMatch(/values cannot contain/i)
  })

  test('rejects duplicate included localStorage key', () => {
    const storageRows = [
      { id: 1, name: 'k', value: 'a', included: true },
      { id: 2, name: 'k', value: 'b', included: true },
    ]
    const err = validateBrowserData(make({ storageRows }))
    expect(err).toMatch(/Duplicate localStorage key/)
  })

  test('passes at exactly 65536 cookie bytes', () => {
    // 8 cookies: name 1 byte ('k') + value 8191 bytes = 8192 per cookie;
    // 8 × 8192 = 65536 total (exact limit, each value ≤ MAX_COOKIE_VALUE_BYTES).
    const val = 'x'.repeat(8191)
    const cookieRows = Array.from({ length: 8 }, (_, i) => ({
      id: i + 1, name: `k${i}`, value: val.slice(0, 8191 - String(i).length), included: true,
    }))
    // Simplest: just 8 single-char-name cookies to hit exactly 65536
    const rows = Array.from({ length: 8 }, (_, i) => ({
      id: i, name: String.fromCharCode(97 + i), value: 'x'.repeat(8191), included: true,
    }))
    // 8 × (1 + 8191) = 8 × 8192 = 65536
    expect(validateBrowserData(make({ cookieRows: rows }))).toBeNull()
  })

  test('fails above 65536 cookie bytes', () => {
    // 9 × 8192 = 73728 > 65536
    const rows = Array.from({ length: 9 }, (_, i) => ({
      id: i, name: String.fromCharCode(97 + i), value: 'x'.repeat(8191), included: true,
    }))
    const err = validateBrowserData(make({ cookieRows: rows }))
    expect(err).toMatch(/64 KiB/)
  })

  test('skips cookie validation when useCookies is false', () => {
    const cookieRows = [{ id: 1, name: 'a b', value: 'x', included: true }]
    expect(validateBrowserData(make({ useCookies: false, cookieRows }))).toBeNull()
  })

  test('skips storage validation when includeStorage is false', () => {
    const storageRows = [
      { id: 1, name: 'k', value: 'a', included: true },
      { id: 2, name: 'k', value: 'b', included: true },
    ]
    expect(validateBrowserData(make({ includeStorage: false, storageRows }))).toBeNull()
  })
})

describe('buildBrowserSessionPayload', () => {
  test('omits excluded rows', () => {
    const cookieRows = [
      { id: 1, name: 'a', value: 'x', included: true },
      { id: 2, name: 'b', value: 'y', included: false },
    ]
    const payload = buildBrowserSessionPayload({
      pageUrl: 'https://example.com/',
      useCookies: true,
      includeStorage: false,
      cookieRows,
      storageRows: [],
    })
    expect(payload).not.toBeNull()
    expect(Object.keys(payload.cookies)).toEqual(['a'])
    expect(payload.local_storage).toEqual({})
  })

  test('returns null when both maps are empty', () => {
    const payload = buildBrowserSessionPayload({
      pageUrl: 'https://example.com/',
      useCookies: true,
      includeStorage: true,
      cookieRows: [],
      storageRows: [],
    })
    expect(payload).toBeNull()
  })

  test('returns null when both toggles are off', () => {
    const cookieRows = [{ id: 1, name: 'a', value: 'x', included: true }]
    const storageRows = [{ id: 2, name: 'k', value: 'v', included: true }]
    const payload = buildBrowserSessionPayload({
      pageUrl: 'https://example.com/',
      useCookies: false,
      includeStorage: false,
      cookieRows,
      storageRows,
    })
    expect(payload).toBeNull()
  })

  test('includes url in payload', () => {
    const cookieRows = [{ id: 1, name: 'a', value: 'x', included: true }]
    const payload = buildBrowserSessionPayload({
      pageUrl: 'https://example.com/page',
      useCookies: true,
      includeStorage: false,
      cookieRows,
      storageRows: [],
    })
    expect(payload.url).toBe('https://example.com/page')
  })
})
