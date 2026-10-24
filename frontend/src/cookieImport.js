/**
 * Converts cookies pasted into Settings › Cookies into the cookie-rule format
 * the server accepts: a JSON object whose values are all strings
 * ({"name": "value"}).
 *
 * Recognised inputs:
 *   - archivr JSON:   {"session": "abc"}
 *   - extension JSON: [{"name": "session", "value": "abc", "domain": …}, …]
 *                     (Cookie-Editor / EditThisCookie export; a single such object also works)
 *   - Netscape:       cookies.txt lines, including `#HttpOnly_` prefixed ones
 *   - header string:  session=abc; theme=dark   (optional `Cookie:` prefix)
 *
 * Rules are scoped by URL pattern, so domain/path/expiry/flags are dropped.
 */

const FORMAT_LABELS = {
  archivr: 'archivr JSON',
  extension: 'extension JSON',
  netscape: 'Netscape',
  header: 'header string',
}

// Same character rules as the extension session cookies (browserData.js).
const INVALID_NAME = /[\x00-\x1f\x7f ;=]/
const INVALID_VALUE = /[\x00-\x1f\x7f;]/

const NETSCAPE_HEADER = /^#\s*Netscape HTTP Cookie File/i
const HTTP_ONLY_PREFIX = '#HttpOnly_'
const BOOL_FIELD = /^(TRUE|FALSE)$/i

function fail(error) {
  return { ok: false, error }
}

function checkPair(name, value) {
  if (!name || INVALID_NAME.test(name)) {
    return `Invalid cookie name ${JSON.stringify(name)}: names cannot be empty or contain spaces, ';' or '='.`
  }
  if (INVALID_VALUE.test(value)) {
    return `Invalid value for cookie ${JSON.stringify(name)}: values cannot contain ';' or control characters.`
  }
  return null
}

function converted(format, entries) {
  for (const [name, value] of entries) {
    const err = checkPair(name, value)
    if (err) return fail(err)
  }
  if (entries.length === 0) return fail(`No cookies found in ${FORMAT_LABELS[format]}.`)
  return { ok: true, format, cookies: entries }
}

function isPlainObject(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v)
}

function extensionCookie(item, index) {
  if (!isPlainObject(item) || typeof item.name !== 'string' || typeof item.value !== 'string') {
    return { error: `Cookie #${index + 1} needs string "name" and "value" fields.` }
  }
  return { entry: [item.name, item.value] }
}

function parseJson(text) {
  let data
  try { data = JSON.parse(text) } catch {
    return fail('Invalid JSON.')
  }
  if (Array.isArray(data)) {
    const entries = []
    for (let i = 0; i < data.length; i++) {
      const { entry, error } = extensionCookie(data[i], i)
      if (error) return fail(error)
      entries.push(entry)
    }
    return converted('extension', entries)
  }
  if (!isPlainObject(data)) {
    return fail('Cookies JSON must be an object like {"session": "abc"} or an extension export array.')
  }
  const values = Object.values(data)
  if (values.every(v => typeof v === 'string')) {
    // Native format: passed through as-is (the server is the authority on it).
    return { ok: true, format: 'archivr', cookies: Object.entries(data) }
  }
  // One cookie copied out of an extension export ({"name", "value", "domain", …}).
  if (typeof data.name === 'string' && typeof data.value === 'string') {
    return converted('extension', [[data.name, data.value]])
  }
  return fail('Cookie values must all be strings, e.g. {"session": "abc"}.')
}

function netscapeFields(line) {
  const tabbed = line.split('\t')
  if (tabbed.length >= 7) return tabbed
  // Copies out of a terminal or chat often turn tabs into spaces.
  return line.trim().split(/\s+/)
}

function looksLikeNetscapeLine(line) {
  const fields = netscapeFields(line)
  return fields.length >= 6 && BOOL_FIELD.test(fields[1]) && BOOL_FIELD.test(fields[3])
}

function cookieLines(text) {
  return text.split(/\r?\n/)
    .map(line => line.trimEnd())
    .filter(line => line.trim() !== '')
    .map(line => line.startsWith(HTTP_ONLY_PREFIX) ? line.slice(HTTP_ONLY_PREFIX.length) : line)
    .filter(line => !line.startsWith('#'))
}

function parseNetscape(text) {
  const entries = []
  for (const line of cookieLines(text)) {
    if (!looksLikeNetscapeLine(line)) {
      return fail(`Not a Netscape cookie line: ${line.slice(0, 80)}`)
    }
    const fields = netscapeFields(line)
    const sep = fields.length === line.split('\t').length ? '\t' : ' '
    entries.push([fields[5], fields.slice(6).join(sep)])
  }
  return converted('netscape', entries)
}

function parseHeader(text) {
  const body = text.trim().replace(/^cookie:\s*/i, '')
  const entries = []
  for (const part of body.split(/;|\r?\n/)) {
    const pair = part.trim()
    if (pair === '') continue
    const eq = pair.indexOf('=')
    if (eq <= 0) return fail(`Expected name=value, got ${JSON.stringify(pair.slice(0, 80))}.`)
    entries.push([pair.slice(0, eq).trim(), pair.slice(eq + 1).trim()])
  }
  return converted('header', entries)
}

/**
 * Parses cookies in any supported format.
 * @param {string} text
 * @returns {{ok: true, format: string, cookies: Array<[string, string]>} | {ok: false, error: string}}
 */
export function parseCookies(text) {
  const trimmed = (text ?? '').trim()
  if (trimmed === '') return fail('No cookies found.')
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) return parseJson(trimmed)
  const firstLine = cookieLines(trimmed)[0]
  if (NETSCAPE_HEADER.test(trimmed) || (firstLine !== undefined && looksLikeNetscapeLine(firstLine))) {
    return parseNetscape(trimmed)
  }
  return parseHeader(trimmed)
}

/**
 * Merges cookie lists; `incoming` wins on a name collision.
 * @returns {{cookies: Object<string, string>, added: number, replaced: number}}
 */
export function mergeCookies(base, incoming) {
  const merged = new Map(base)
  let added = 0
  let replaced = 0
  for (const [name, value] of incoming) {
    if (!merged.has(name)) added++
    else if (merged.get(name) !== value) replaced++
    merged.set(name, value)
  }
  // Object.fromEntries keeps names such as "__proto__" as own properties.
  return { cookies: Object.fromEntries(merged), added, replaced }
}

export function formatCookies(cookies) {
  return JSON.stringify(cookies, null, 2)
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? '' : 's'}`
}

/**
 * Normalises a cookie-rule textarea before it is sent to the server.
 * Native JSON is sent unchanged; other formats are converted.
 * @returns {{ok: true, json: string} | {ok: false, error: string}}
 */
export function normalizeCookiesInput(text) {
  const parsed = parseCookies(text)
  if (!parsed.ok) return parsed
  if (parsed.format === 'archivr') return { ok: true, json: text }
  return { ok: true, json: formatCookies(Object.fromEntries(parsed.cookies)) }
}

/**
 * Decides what pasting `clip` over the selection [start, end) of `current` does.
 *
 * Returns null when the browser's normal paste should run: the result is
 * already a valid cookie object (ordinary JSON editing), or the clipboard is
 * not cookies at all. Otherwise the caller prevents the paste and either
 * replaces the textarea with `text` or shows `error`.
 *
 * @returns {null | {text: string, message: string} | {error: string}}
 */
export function pasteCookies(current, start, end, clip) {
  const before = current.slice(0, start)
  const after = current.slice(end)
  const combined = parseCookies(before + clip + after)
  if (combined.ok && combined.format === 'archivr') return null

  const incoming = parseCookies(clip)
  if (!incoming.ok) return null

  const label = FORMAT_LABELS[incoming.format]
  const count = plural(incoming.cookies.length, 'cookie')
  const rest = before + after
  if (rest.trim() === '') {
    return {
      text: formatCookies(Object.fromEntries(incoming.cookies)),
      message: `Converted ${count} from ${label}.`,
    }
  }

  const existing = parseCookies(rest)
  if (!existing.ok) {
    return {
      error: `Clipboard has ${count} (${label}), but the existing content isn't valid cookies (${existing.error}) — fix or clear it, then paste again.`,
    }
  }
  const { cookies, added, replaced } = mergeCookies(existing.cookies, incoming.cookies)
  const message = existing.cookies.length === 0
    ? `Converted ${count} from ${label}.`
    : `Merged ${count} from ${label}: ${added} added, ${replaced} replaced.`
  return { text: formatCookies(cookies), message }
}
