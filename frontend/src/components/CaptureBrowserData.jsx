import { toRows } from '../browserData.js'

/**
 * The "Use my cookies" toggle row rendered below the Add URL / Add text buttons.
 * Only shown when `browserSession.appliesTo` is true for at least one URL item.
 *
 * Props:
 *   browserSession – the browserSession prop from CaptureDialog
 *   browserData    – { status, cookieAccess, cookieRows, storageRows }
 *   useCookies     – boolean
 *   setUseCookies  – setter
 */
export function BrowserSessionToggle({ browserSession, browserData, useCookies, setUseCookies }) {
  const { status, cookieAccess, cookieRows } = browserData
  const host = browserSession?.host ?? ''
  const includedCount = (cookieRows || []).filter(r => r.included).length

  const loading = status === 'loading' || status === 'idle'
  const accessMissing = cookieAccess === 'missing'
  const hasError = status === 'error'
  const disabled = loading || accessMissing || hasError

  let metaContent
  if (loading) {
    metaContent = <span className="capture-browser-session-meta">Reading cookies…</span>
  } else if (hasError) {
    metaContent = <span className="capture-browser-session-meta">Couldn&apos;t read cookies</span>
  } else if (accessMissing) {
    metaContent = (
      <>
        <button
          type="button"
          className="capture-browser-session-grant"
          onClick={() => browserSession.requestAccess()}
        >
          Allow cookie access
        </button>
      </>
    )
  } else {
    const cookieText = includedCount === 0 ? 'no cookies' : `${includedCount} ${includedCount === 1 ? 'cookie' : 'cookies'}`
    metaContent = <span className="capture-browser-session-meta">{host} · {cookieText}</span>
  }

  return (
    <label className="capture-browser-session-row">
      <input
        type="checkbox"
        checked={accessMissing || hasError ? false : useCookies}
        disabled={disabled}
        onChange={e => setUseCookies(e.target.checked)}
      />
      <span className="capture-browser-session-label">Use my cookies</span>
      {metaContent}
    </label>
  )
}

/**
 * The browser data section rendered at the bottom of Advanced options.
 * Shows Include localStorage toggle, Cookies table, localStorage table.
 *
 * Props:
 *   browserSession   – the browserSession prop from CaptureDialog
 *   browserData      – { status, cookieAccess, cookieRows, storageRows }
 *   setBrowserData   – setter
 *   useCookies       – boolean
 *   effectiveUseCookies – boolean (useCookies && access granted)
 *   includeStorage   – boolean
 *   setIncludeStorage – setter
 *   browserDataError – string|null
 */
export function BrowserDataSection({
  browserSession,
  browserData,
  setBrowserData,
  effectiveUseCookies,
  includeStorage,
  storageApplicable,
  setIncludeStorage,
  browserDataError,
}) {
  const host = browserSession?.host ?? ''
  const { cookieRows = [], storageRows = [] } = browserData

  function updateCookieRow(id, field, value) {
    setBrowserData(prev => ({
      ...prev,
      cookieRows: prev.cookieRows.map(r => r.id === id ? { ...r, [field]: value } : r),
    }))
  }

  function removeCookieRow(id) {
    setBrowserData(prev => ({
      ...prev,
      cookieRows: prev.cookieRows.filter(r => r.id !== id),
    }))
  }

  function addCookieRow() {
    const newRow = { id: Date.now() + Math.random(), name: '', value: '', included: true }
    setBrowserData(prev => ({ ...prev, cookieRows: [...prev.cookieRows, newRow] }))
  }

  function updateStorageRow(id, field, value) {
    setBrowserData(prev => ({
      ...prev,
      storageRows: prev.storageRows.map(r => r.id === id ? { ...r, [field]: value } : r),
    }))
  }

  function removeStorageRow(id) {
    setBrowserData(prev => ({
      ...prev,
      storageRows: prev.storageRows.filter(r => r.id !== id),
    }))
  }

  function addStorageRow() {
    const newRow = { id: Date.now() + Math.random(), name: '', value: '', included: true }
    setBrowserData(prev => ({ ...prev, storageRows: [...prev.storageRows, newRow] }))
  }

  const cookieIncluded = cookieRows.filter(r => r.included).length
  const storageIncluded = storageRows.filter(r => r.included).length

  return (
    <div className="capture-browser-data">
      <div className="capture-browser-data-title">Browser data · {host}</div>

      {/* Include localStorage toggle */}
      <label className="capture-ext-row" style={{ marginTop: 8 }}>
        <span className="capture-ext-label">
          <span className="capture-ext-name">Include localStorage</span>
          <span className="capture-ext-desc">
            {storageApplicable
              ? 'Seed this site\'s localStorage in the archiving browser (page captures only)'
              : 'Available only for URLs on this page\'s exact origin (scheme, host, and port)'}
          </span>
        </span>
        <button
          type="button"
          role="switch"
          aria-checked={includeStorage}
          disabled={!storageApplicable}
          className={`ext-toggle ext-toggle--sm${includeStorage ? ' ext-toggle--on' : ''}`}
          onClick={() => setIncludeStorage(v => !v)}
          aria-label="Toggle localStorage for this capture"
        >
          <span className="ext-toggle-knob" />
        </button>
      </label>

      {/* Cookies table */}
      {effectiveUseCookies && (
        <div className="capture-kv">
          <div className="capture-kv-head">
            <span>Cookies</span>
            <span>{cookieIncluded} of {cookieRows.length} included</span>
          </div>
          <div className="capture-kv-scroll">
            <table className="capture-kv-table">
              <thead>
                <tr>
                  <th style={{ width: 28 }}></th>
                  <th style={{ width: '34%' }}>Name</th>
                  <th>Value</th>
                  <th style={{ width: 28 }}></th>
                </tr>
              </thead>
              <tbody>
                {cookieRows.map(row => (
                  <tr key={row.id} className={row.included ? '' : 'capture-kv-row--excluded'}>
                    <td>
                      <input
                        type="checkbox"
                        checked={row.included}
                        onChange={e => updateCookieRow(row.id, 'included', e.target.checked)}
                        aria-label={`Include ${row.name}`}
                      />
                    </td>
                    <td>
                      <input
                        className="capture-kv-input"
                        value={row.name}
                        onChange={e => updateCookieRow(row.id, 'name', e.target.value)}
                        placeholder="name"
                        aria-label="Cookie name"
                      />
                      {row.httpOnly && <span className="capture-kv-badge">HttpOnly</span>}
                    </td>
                    <td>
                      <input
                        className="capture-kv-input"
                        value={row.value}
                        onChange={e => updateCookieRow(row.id, 'value', e.target.value)}
                        placeholder="value"
                        aria-label="Cookie value"
                      />
                    </td>
                    <td>
                      <button
                        type="button"
                        className="capture-kv-remove"
                        onClick={() => removeCookieRow(row.id)}
                        aria-label={`Remove ${row.name}`}
                      >×</button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <button type="button" className="capture-add-row capture-kv-add" onClick={addCookieRow}>
            + Add cookie
          </button>
        </div>
      )}

      {/* localStorage table */}
      {includeStorage && (
        <div className="capture-kv">
          <div className="capture-kv-head">
            <span>localStorage</span>
            <span>{storageIncluded} of {storageRows.length} included</span>
          </div>
          <div className="capture-kv-scroll">
            <table className="capture-kv-table">
              <thead>
                <tr>
                  <th style={{ width: 28 }}></th>
                  <th style={{ width: '34%' }}>Key</th>
                  <th>Value</th>
                  <th style={{ width: 28 }}></th>
                </tr>
              </thead>
              <tbody>
                {storageRows.map(row => (
                  <tr key={row.id} className={row.included ? '' : 'capture-kv-row--excluded'}>
                    <td>
                      <input
                        type="checkbox"
                        checked={row.included}
                        onChange={e => updateStorageRow(row.id, 'included', e.target.checked)}
                        aria-label={`Include ${row.name}`}
                      />
                    </td>
                    <td>
                      <input
                        className="capture-kv-input"
                        value={row.name}
                        onChange={e => updateStorageRow(row.id, 'name', e.target.value)}
                        placeholder="key"
                        aria-label="localStorage key"
                      />
                    </td>
                    <td>
                      <input
                        className="capture-kv-input"
                        value={row.value}
                        onChange={e => updateStorageRow(row.id, 'value', e.target.value)}
                        placeholder="value"
                        aria-label="localStorage value"
                      />
                    </td>
                    <td>
                      <button
                        type="button"
                        className="capture-kv-remove"
                        onClick={() => removeStorageRow(row.id)}
                        aria-label={`Remove ${row.name}`}
                      >×</button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <button type="button" className="capture-add-row capture-kv-add" onClick={addStorageRow}>
            + Add item
          </button>
        </div>
      )}

      {browserDataError && (
        <p className="capture-row-error capture-browser-data-error" role="alert">
          {browserDataError}
        </p>
      )}
    </div>
  )
}
