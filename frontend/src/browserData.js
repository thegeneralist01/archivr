/**
 * Pure helpers for browser session data (cookies + localStorage) forwarded
 * from the extension's in-page capture dialog.
 *
 * Limits mirror the Rust server (browser_session.rs) and the extension (core.ts).
 */

// ── Limits (mirrored in browser_session.rs and extension core.ts) ─────────────

const MAX_COOKIES = 300
const MAX_COOKIE_NAME_BYTES = 256
const MAX_COOKIE_VALUE_BYTES = 8192
const MAX_COOKIES_TOTAL_BYTES = 65536
const MAX_STORAGE_ITEMS = 2000
const MAX_STORAGE_KEY_BYTES = 1024
const MAX_STORAGE_TOTAL_BYTES = 1_048_576

let _rowCounter = 0

/** @returns {number} */
function nextRowId() {
  return ++_rowCounter
}

/**
 * Maps a flat list of objects to table rows.
 * @param {Array<object>} list  Items with a name/key field and a value field.
 * @param {'name'|'key'} keyField  Name of the property holding the key.
 * @returns {Array<{id:number, name:string, value:string, included:boolean, httpOnly?:boolean}>}
 */
export function toRows(list, keyField) {
  return (list || []).map(item => ({
    id: nextRowId(),
    name: item[keyField] ?? '',
    value: item.value ?? '',
    included: true,
    ...(keyField === 'name' && item.httpOnly !== undefined ? { httpOnly: !!item.httpOnly } : {}),
  }))
}

/**
 * Converts rows to a plain object of included rows only.
 * @param {Array<{name:string, value:string, included:boolean}>} rows
 * @returns {Object<string,string>}
 */
export function rowsToMap(rows) {
  // Object.fromEntries creates an own property for names such as "__proto__".
  // Assignment into {} silently drops that valid cookie/localStorage key.
  return Object.fromEntries(rows
    .filter(row => row.included && row.name !== '')
    .map(row => [row.name, row.value]))
}

/** @param {string} s */
function byteLength(s) {
  return new TextEncoder().encode(s).length
}

/**
 * Validates browser data.
 * Returns the first error string, or `null` if everything is valid.
 * Only included rows count, and only for enabled kinds.
 *
 * @param {{
 *   useCookies: boolean,
 *   includeStorage: boolean,
 *   cookieRows: Array<{name:string, value:string, included:boolean}>,
 *   storageRows: Array<{name:string, value:string, included:boolean}>
 * }} opts
 * @returns {string|null}
 */
export function validateBrowserData({ useCookies, includeStorage, cookieRows, storageRows }) {
  if (useCookies) {
    const included = (cookieRows || []).filter(r => r.included)
    if (included.length > MAX_COOKIES) {
      return `Too many cookies (max ${MAX_COOKIES}).`
    }
    // Duplicate name check
    const seen = new Set()
    for (const row of included) {
      if (seen.has(row.name)) {
        return `Duplicate cookie name: ${row.name}`
      }
      seen.add(row.name)
    }
    // Per-row validation
    for (const row of included) {
      const name = row.name
      const value = row.value
      if (!name || byteLength(name) > MAX_COOKIE_NAME_BYTES || /[\x00-\x1f\x7f ;=]/.test(name)) {
        return `Cookie names cannot be empty or contain spaces, ';' or '='.`
      }
      if (byteLength(value) > MAX_COOKIE_VALUE_BYTES || /[\x00-\x1f\x7f;]/.test(value)) {
        return `Cookie values cannot contain ';' or control characters.`
      }
    }
    // Total byte check
    let total = 0
    for (const row of included) {
      total += byteLength(row.name) + byteLength(row.value)
    }
    if (total > MAX_COOKIES_TOTAL_BYTES) {
      return `Cookies exceed 64 KiB — exclude some.`
    }
  }

  if (includeStorage) {
    const included = (storageRows || []).filter(r => r.included)
    if (included.length > MAX_STORAGE_ITEMS) {
      return `Too many localStorage items (max ${MAX_STORAGE_ITEMS}).`
    }
    // Duplicate key check
    const seen = new Set()
    for (const row of included) {
      if (seen.has(row.name)) {
        return `Duplicate localStorage key: ${row.name}`
      }
      seen.add(row.name)
    }
    // Per-row key validation
    for (const row of included) {
      if (!row.name || byteLength(row.name) > MAX_STORAGE_KEY_BYTES) {
        return `localStorage keys cannot be empty or longer than 1024 bytes.`
      }
    }
    // Total byte check
    let total = 0
    for (const row of included) {
      total += byteLength(row.name) + byteLength(row.value)
    }
    if (total > MAX_STORAGE_TOTAL_BYTES) {
      return `localStorage exceeds 1 MiB — exclude some.`
    }
  }

  return null
}

/**
 * Builds the `browser_session` payload for the capture API.
 * Returns `null` when both resulting maps are empty (nothing to send).
 *
 * @param {{
 *   pageUrl: string,
 *   useCookies: boolean,
 *   includeStorage: boolean,
 *   cookieRows: Array<{name:string, value:string, included:boolean}>,
 *   storageRows: Array<{name:string, value:string, included:boolean}>
 * }} opts
 * @returns {{ url: string, cookies: object, local_storage: object }|null}
 */
export function buildBrowserSessionPayload({ pageUrl, useCookies, includeStorage, cookieRows, storageRows }) {
  const cookies = useCookies ? rowsToMap(cookieRows || []) : {}
  const local_storage = includeStorage ? rowsToMap(storageRows || []) : {}
  if (Object.keys(cookies).length === 0 && Object.keys(local_storage).length === 0) {
    return null
  }
  return { url: pageUrl, cookies, local_storage }
}
