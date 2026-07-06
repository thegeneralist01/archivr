import { useState, useEffect, useContext, useCallback } from 'react'
import { AuthContext } from '../App.jsx'
import {
  updateProfile, changePassword, patchMe,
  listTokens, createToken, deleteToken,
  getInstanceSettings, updateInstanceSettings,
  scanOrphanBlobs, deleteOrphanBlobs,
  listCookieRules, createCookieRule, updateCookieRule, deleteCookieRule,
} from '../api.js'

const ROLE_ADMIN = 4

export default function SettingsView({ tab, onTabChange, archiveId }) {
  const { currentUser, setCurrentUser } = useContext(AuthContext) ?? {}
  const isAdmin = currentUser && ((currentUser.role_bits & ROLE_ADMIN) !== 0)

  const tabs = ['profile', 'tokens', ...(isAdmin ? ['instance', 'cookies', 'storage'] : [])]
  const tabLabels = { profile: 'Profile', tokens: 'API Tokens', instance: 'Instance', cookies: 'Cookies', storage: 'Storage' }

  return (
    <section className="admin-view">
      <h1>Settings</h1>
      <div className="view-tabs">
        {tabs.map(t => (
          <button key={t}
            className={`view-tab${tab === t ? ' is-active' : ''}`}
            onClick={() => onTabChange(t)}>
            {tabLabels[t]}
          </button>
        ))}
      </div>

      {tab === 'profile' && <ProfileTab currentUser={currentUser} setCurrentUser={setCurrentUser} />}
      {tab === 'tokens' && <TokensTab />}
      {tab === 'instance' && isAdmin && <InstanceTab />}
      {tab === 'cookies' && isAdmin && <CookiesTab />}
      {tab === 'storage' && isAdmin && <StorageTab archiveId={archiveId} />}
    </section>
  )
}

function ProfileTab({ currentUser, setCurrentUser }) {
  const [displayName, setDisplayName] = useState(currentUser?.display_name ?? '')
  const [saving, setSaving] = useState(false)
  const [saveMsg, setSaveMsg] = useState(null)

  const [curPw, setCurPw] = useState('')
  const [newPw, setNewPw] = useState('')
  const [confirmPw, setConfirmPw] = useState('')
  const [pwSaving, setPwSaving] = useState(false)
  const [pwMsg, setPwMsg] = useState(null)

  async function handleSaveProfile(e) {
    e.preventDefault()
    setSaving(true)
    setSaveMsg(null)
    try {
      await updateProfile(displayName)
      setCurrentUser(u => ({ ...u, display_name: displayName || null }))
      setSaveMsg({ ok: true, text: 'Saved.' })
    } catch (err) {
      setSaveMsg({ ok: false, text: err.message })
    } finally {
      setSaving(false)
    }
  }

  async function handleChangePassword(e) {
    e.preventDefault()
    if (newPw !== confirmPw) { setPwMsg({ ok: false, text: 'Passwords do not match.' }); return }
    setPwSaving(true)
    setPwMsg(null)
    try {
      await changePassword(curPw, newPw)
      setCurPw(''); setNewPw(''); setConfirmPw('')
      setPwMsg({ ok: true, text: 'Password changed.' })
    } catch (err) {
      setPwMsg({ ok: false, text: err.message })
    } finally {
      setPwSaving(false)
    }
  }

  return (
    <div style={{ maxWidth: 440 }}>
      <div className="form-section">
        <h2>Display Name</h2>
        <form onSubmit={handleSaveProfile}>
          <div className="form-field">
            <label className="form-label" htmlFor="display-name">Name shown in the UI</label>
            <input className="field-input" id="display-name"
              placeholder={currentUser?.username ?? ''}
              value={displayName} onChange={e => setDisplayName(e.target.value)} />
          </div>
          {saveMsg && <div className={`form-msg form-msg--${saveMsg.ok ? 'ok' : 'err'}`}>{saveMsg.text}</div>}
          <button className="btn-primary" type="submit" disabled={saving}>
            {saving ? 'Saving\u2026' : 'Save'}
          </button>
        </form>
      </div>

      <div className="form-section">
        <h2>Display Preferences</h2>
        <label className="checkbox-row">
          <input
            type="checkbox"
            checked={currentUser?.humanize_slugs ?? false}
            onChange={async e => {
              const checked = e.target.checked;
              try {
                await patchMe({ humanize_slugs: checked });
                setCurrentUser(prev => ({ ...prev, humanize_slugs: checked }));
              } catch {
                // silently revert
              }
            }}
          />
          <span className="form-label" style={{ margin: 0 }}>Humanize tag display</span>
        </label>
        <p className="muted" style={{ fontSize: 13, margin: '4px 0 0' }}>
          When on, tag paths show as "X / Articles" instead of "x/articles".
        </p>
      </div>

      <div className="form-section">
        <h2>Change Password</h2>
        <form onSubmit={handleChangePassword}>
          <div className="form-field">
            <label className="form-label" htmlFor="cur-pw">Current password</label>
            <input className="field-input" id="cur-pw" type="password"
              value={curPw} onChange={e => setCurPw(e.target.value)} required />
          </div>
          <div className="form-field">
            <label className="form-label" htmlFor="new-pw">New password</label>
            <input className="field-input" id="new-pw" type="password"
              value={newPw} onChange={e => setNewPw(e.target.value)} required />
          </div>
          <div className="form-field">
            <label className="form-label" htmlFor="confirm-pw">Confirm new password</label>
            <input className="field-input" id="confirm-pw" type="password"
              value={confirmPw} onChange={e => setConfirmPw(e.target.value)} required />
          </div>
          {pwMsg && <div className={`form-msg form-msg--${pwMsg.ok ? 'ok' : 'err'}`}>{pwMsg.text}</div>}
          <button className="btn-primary" type="submit" disabled={pwSaving}>
            {pwSaving ? 'Changing\u2026' : 'Change Password'}
          </button>
        </form>
      </div>
    </div>
  )
}

function TokensTab() {
  const [tokens, setTokens] = useState([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)
  const [newName, setNewName] = useState('')
  const [creating, setCreating] = useState(false)
  const [newToken, setNewToken] = useState(null)

  const refresh = useCallback(async () => {
    setLoading(true); setError(null)
    try { setTokens(await listTokens()) }
    catch (e) { setError(e.message) }
    finally { setLoading(false) }
  }, [])

  useEffect(() => { refresh() }, [refresh])

  async function handleCreate(e) {
    e.preventDefault()
    if (!newName.trim()) return
    setCreating(true)
    try {
      const tok = await createToken(newName.trim())
      setNewToken(tok)
      setNewName('')
      refresh()
    } catch (err) {
      setError(err.message)
    } finally {
      setCreating(false)
    }
  }

  async function handleDelete(tokenUid) {
    try {
      await deleteToken(tokenUid)
      setTokens(ts => ts.filter(t => t.token_uid !== tokenUid))
    } catch (err) {
      setError(err.message)
    }
  }

  return (
    <div style={{ maxWidth: 600 }}>
      <div className="form-section">
        <h2>API Tokens</h2>
        {newToken && (
          <div className="token-banner">
            <strong>Token created.</strong> Copy it now — it won't be shown again.
            <code>{newToken.raw_token}</code>
            <button className="token-dismiss" onClick={() => setNewToken(null)}>Dismiss</button>
          </div>
        )}
        <form className="token-create-row" onSubmit={handleCreate}>
          <input className="field-input field-input--flex" placeholder="Token name"
            value={newName} onChange={e => setNewName(e.target.value)} required />
          <button className="btn-primary" type="submit" disabled={creating}>
            {creating ? 'Creating\u2026' : 'Create token'}
          </button>
        </form>
        {error && <div className="form-msg form-msg--err">{error}</div>}
        {loading ? (
          <div className="muted">Loading…</div>
        ) : (
          <div>
            {tokens.length === 0 && <div className="muted">No tokens yet.</div>}
            {tokens.map(tok => (
              <div key={tok.token_uid} className="token-row">
                <div className="token-row-info">
                  <strong>{tok.name}</strong>
                  <div className="muted">
                    Created {tok.created_at.slice(0, 10)}
                    {tok.last_used_at && ` \u00b7 Last used ${tok.last_used_at.slice(0, 10)}`}
                  </div>
                </div>
                <button className="btn-danger" style={{ fontSize: 12, padding: '4px 10px' }}
                  onClick={() => handleDelete(tok.token_uid)}>
                  Revoke
                </button>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

function InstanceTab() {
  const [settings, setSettings] = useState(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)
  const [saving, setSaving] = useState(false)
  const [saveMsg, setSaveMsg] = useState(null)

  useEffect(() => {
    (async () => {
      try { setSettings(await getInstanceSettings()) }
      catch (e) { setError(e.message) }
      finally { setLoading(false) }
    })()
  }, [])

  async function handleSave(e) {
    e.preventDefault()
    setSaving(true); setSaveMsg(null)
    try {
      await updateInstanceSettings(settings)
      setSaveMsg({ ok: true, text: 'Saved.' })
    } catch (err) {
      setSaveMsg({ ok: false, text: err.message })
    } finally {
      setSaving(false)
    }
  }

  if (loading) return <div className="muted">Loading…</div>
  if (error) return <div className="form-msg form-msg--err">{error}</div>
  if (!settings) return null

  return (
    <div style={{ maxWidth: 440 }}>
      <div className="form-section">
        <h2>Instance Settings</h2>
        <form onSubmit={handleSave}>
          {[
            ['public_index_enabled', 'Public index (unauthenticated browsing)'],
            ['public_entry_content_enabled', 'Public entry content'],
            ['open_registration_enabled', 'Open registration'],
          ].map(([key, label]) => (
            <label key={key} className="checkbox-row">
              <input type="checkbox" checked={!!settings[key]}
                onChange={e => setSettings(s => ({ ...s, [key]: e.target.checked }))} />
              {label}
            </label>
          ))}
          <div className="form-field" style={{ marginTop: 4 }}>
            <label className="form-label">Default entry visibility</label>
            <select className="field-input" value={settings.default_entry_visibility}
              onChange={e => setSettings(s => ({ ...s, default_entry_visibility: Number(e.target.value) }))}>
              <option value={0}>Private</option>
              <option value={2}>Unlisted</option>
              <option value={3}>Public</option>
            </select>
          </div>
          {saveMsg && <div className={`form-msg form-msg--${saveMsg.ok ? 'ok' : 'err'}`}>{saveMsg.text}</div>}
          <button className="btn-primary" type="submit" disabled={saving}>
            {saving ? 'Saving\u2026' : 'Save Settings'}
          </button>
        </form>
      </div>
    </div>
  )
}

function formatBytes(bytes) {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}

function StorageTab({ archiveId }) {
  // phase: 'idle' | 'scanning' | 'scanned' | 'deleting' | 'done' | 'error'
  const [phase, setPhase] = useState('idle')
  const [scanResult, setScanResult] = useState(null)
  const [deleteResult, setDeleteResult] = useState(null)
  const [errorMsg, setErrorMsg] = useState(null)

  function reset() {
    setPhase('idle')
    setScanResult(null)
    setDeleteResult(null)
    setErrorMsg(null)
  }

  async function handleScan() {
    setPhase('scanning')
    setErrorMsg(null)
    setScanResult(null)
    try {
      const result = await scanOrphanBlobs(archiveId)
      setScanResult(result)
      setPhase('scanned')
    } catch (e) {
      setErrorMsg(e.message)
      setPhase('error')
    }
  }

  async function handleDelete() {
    setPhase('deleting')
    setErrorMsg(null)
    try {
      const result = await deleteOrphanBlobs(archiveId)
      setDeleteResult(result)
      setPhase('done')
    } catch (e) {
      setErrorMsg(e.message)
      setPhase('error')
    }
  }

  const nothing = scanResult && scanResult.deletable_files === 0 && scanResult.orphaned_blob_rows === 0

  return (
    <div style={{ maxWidth: 440 }}>
      <div className="form-section">
        <h2>Orphan Cleanup</h2>
        <p className="muted" style={{ marginBottom: 16 }}>
          Scan for blob files and database records that are no longer referenced by
          any archive entry and safely delete them.
          {' '}<strong>Cleanup is blocked while captures are running.</strong>
        </p>

        {!archiveId && (
          <div className="muted">No archive selected.</div>
        )}

        {archiveId && phase === 'idle' && (
          <button className="btn-ghost" onClick={handleScan}>
            Scan for orphaned blobs
          </button>
        )}

        {phase === 'scanning' && (
          <div className="muted">Scanning…</div>
        )}

        {phase === 'scanned' && scanResult && nothing && (
          <>
            <div className="form-msg form-msg--ok">Archive is clean &mdash; nothing to remove.</div>
            <button className="btn-ghost" style={{ marginTop: 10 }} onClick={reset}>Done</button>
          </>
        )}

        {phase === 'scanned' && scanResult && !nothing && (
          <div>
            <div style={{ marginBottom: 14, lineHeight: 1.6 }}>
              Found <strong>{scanResult.deletable_files}</strong> unreferenced file{scanResult.deletable_files !== 1 ? 's' : ''}
              {' '}and <strong>{scanResult.orphaned_blob_rows}</strong> orphaned DB record{scanResult.orphaned_blob_rows !== 1 ? 's' : ''}
              {' '}&mdash; <strong>{formatBytes(scanResult.total_bytes)}</strong> recoverable.
            </div>
            <div style={{ display: 'flex', gap: 8 }}>
              <button className="btn-danger" onClick={handleDelete}>
                Delete ({formatBytes(scanResult.total_bytes)})
              </button>
              <button className="btn-ghost" onClick={reset}>Cancel</button>
            </div>
          </div>
        )}

        {phase === 'deleting' && (
          <div className="muted">Deleting…</div>
        )}

        {phase === 'done' && deleteResult && (
          <div>
            <div className="form-msg form-msg--ok">
              Freed <strong>{formatBytes(deleteResult.freed_bytes)}</strong>
              {' '}&mdash; removed {deleteResult.deleted_files} file{deleteResult.deleted_files !== 1 ? 's' : ''}
              {' '}and {deleteResult.deleted_blob_rows} DB record{deleteResult.deleted_blob_rows !== 1 ? 's' : ''}.
            </div>
            {deleteResult.errors && deleteResult.errors.length > 0 && (
              <div className="form-msg form-msg--err" style={{ marginTop: 6 }}>
                {deleteResult.errors.length} file{deleteResult.errors.length !== 1 ? 's' : ''} could not be deleted.
              </div>
            )}
            <button className="btn-ghost" style={{ marginTop: 10 }} onClick={reset}>Scan again</button>
          </div>
        )}

        {phase === 'error' && (
          <div>
            <div className="form-msg form-msg--err">{errorMsg}</div>
            <button className="btn-ghost" style={{ marginTop: 10 }} onClick={reset}>Try again</button>
          </div>
        )}
      </div>
    </div>
  )
}


function CookiesTab() {
  const [rules, setRules] = useState(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)

  // Form state for adding a new rule
  const [patternKind, setPatternKind] = useState('global')
  const [urlPattern, setUrlPattern] = useState('')
  const [cookiesInput, setCookiesInput] = useState('{}')
  const [addMsg, setAddMsg] = useState(null)
  const [adding, setAdding] = useState(false)

  // Inline-edit state: ruleUid → { cookiesInput, saving, msg }
  const [edits, setEdits] = useState({})

  useEffect(() => {
    load()
  }, [])

  async function load() {
    setLoading(true); setError(null)
    try { setRules(await listCookieRules()) }
    catch (e) { setError(e.message) }
    finally { setLoading(false) }
  }

  async function handleAdd(e) {
    e.preventDefault()
    // Validate JSON
    try { JSON.parse(cookiesInput) } catch {
      setAddMsg({ ok: false, text: 'cookies must be valid JSON, e.g. {"session": "abc"}' })
      return
    }
    setAdding(true); setAddMsg(null)
    try {
      await createCookieRule(
        patternKind === 'global' ? null : urlPattern.trim(),
        patternKind,
        cookiesInput,
      )
      setUrlPattern('')
      setCookiesInput('{}')
      setPatternKind('global')
      setAddMsg({ ok: true, text: 'Rule added.' })
      await load()
    } catch (err) {
      setAddMsg({ ok: false, text: err.message })
    } finally {
      setAdding(false)
    }
  }

  async function handleDelete(ruleUid) {
    try {
      await deleteCookieRule(ruleUid)
      await load()
    } catch (err) {
      setError(err.message)
    }
  }

  function startEdit(rule) {
    setEdits(prev => ({
      ...prev,
      [rule.rule_uid]: {
        cookiesInput: rule.cookies_json,
        saving: false,
        msg: null,
      }
    }))
  }

  function cancelEdit(ruleUid) {
    setEdits(prev => { const n = { ...prev }; delete n[ruleUid]; return n })
  }

  async function saveEdit(rule) {
    const edit = edits[rule.rule_uid]
    try { JSON.parse(edit.cookiesInput) } catch {
      setEdits(prev => ({ ...prev, [rule.rule_uid]: { ...prev[rule.rule_uid], msg: { ok: false, text: 'Invalid JSON' } } }))
      return
    }
    setEdits(prev => ({ ...prev, [rule.rule_uid]: { ...prev[rule.rule_uid], saving: true, msg: null } }))
    try {
      await updateCookieRule(rule.rule_uid, { cookies_json: edit.cookiesInput })
      cancelEdit(rule.rule_uid)
      await load()
    } catch (err) {
      setEdits(prev => ({ ...prev, [rule.rule_uid]: { ...prev[rule.rule_uid], saving: false, msg: { ok: false, text: err.message } } }))
    }
  }

  if (loading) return <div className="muted">Loading&hellip;</div>
  if (error) return <div className="form-msg form-msg--err">{error}</div>

  return (
    <div style={{ maxWidth: 560 }}>
      <div className="form-section">
        <h2>Cookie Rules</h2>
        <p className="muted" style={{ marginBottom: 12 }}>
          Cookies are injected into every capture network request (yt-dlp, HTTP downloads, web-page snapshots).
          Global rules apply to all URLs; wildcard and regex rules apply only to matching URLs.
        </p>

        {rules && rules.length > 0 ? (
          <table className="data-table" style={{ width: '100%', marginBottom: 16 }}>
            <thead>
              <tr>
                <th>Pattern</th>
                <th>Cookies</th>
                <th style={{ width: 100 }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {rules.map(rule => {
                const edit = edits[rule.rule_uid]
                const patternLabel = rule.url_pattern
                  ? <><span className="muted">{rule.pattern_kind}:</span> <code>{rule.url_pattern}</code></>
                  : <span className="muted">global (all URLs)</span>
                return (
                  <tr key={rule.rule_uid}>
                    <td>{patternLabel}</td>
                    <td>
                      {edit ? (
                        <>
                          <textarea
                            className="field-input"
                            style={{ fontFamily: 'monospace', fontSize: 12, width: '100%', minHeight: 60 }}
                            value={edit.cookiesInput}
                            onChange={ev => setEdits(prev => ({ ...prev, [rule.rule_uid]: { ...prev[rule.rule_uid], cookiesInput: ev.target.value } }))}
                          />
                          {edit.msg && <div className={`form-msg form-msg--${edit.msg.ok ? 'ok' : 'err'}`}>{edit.msg.text}</div>}
                          <div style={{ display: 'flex', gap: 8, marginTop: 4 }}>
                            <button className="btn-primary" style={{ fontSize: 12, padding: '2px 8px' }} disabled={edit.saving} onClick={() => saveEdit(rule)}>
                              {edit.saving ? 'Saving\u2026' : 'Save'}
                            </button>
                            <button className="btn-secondary" style={{ fontSize: 12, padding: '2px 8px' }} onClick={() => cancelEdit(rule.rule_uid)}>Cancel</button>
                          </div>
                        </>
                      ) : (
                        <code style={{ fontSize: 12, wordBreak: 'break-all' }}>{rule.cookies_json}</code>
                      )}
                    </td>
                    <td>
                      {!edit && (
                        <div style={{ display: 'flex', gap: 6 }}>
                          <button className="btn-secondary" style={{ fontSize: 12, padding: '2px 8px' }} onClick={() => startEdit(rule)}>Edit</button>
                          <button className="btn-danger" style={{ fontSize: 12, padding: '2px 8px' }} onClick={() => handleDelete(rule.rule_uid)}>Del</button>
                        </div>
                      )}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        ) : (
          <p className="muted" style={{ marginBottom: 16 }}>No cookie rules defined.</p>
        )}

        <h3 style={{ marginBottom: 8 }}>Add Rule</h3>
        <form onSubmit={handleAdd}>
          <div className="form-field">
            <label className="form-label">Pattern type</label>
            <select className="field-input" value={patternKind} onChange={e => setPatternKind(e.target.value)}>
              <option value="global">Global (all URLs)</option>
              <option value="wildcard">Wildcard (e.g. *.youtube.com)</option>
              <option value="regex">Regex (matched against full URL)</option>
            </select>
          </div>
          {patternKind !== 'global' && (
            <div className="form-field">
              <label className="form-label">
                {patternKind === 'wildcard' ? 'URL/hostname pattern' : 'Regex pattern'}
              </label>
              <input
                className="field-input"
                type="text"
                value={urlPattern}
                onChange={e => setUrlPattern(e.target.value)}
                placeholder={patternKind === 'wildcard' ? '*.youtube.com or https://example.com/*' : '.*\\.youtube\\.com.*'}
                required
              />
            </div>
          )}
          <div className="form-field">
            <label className="form-label">Cookies (JSON object)</label>
            <textarea
              className="field-input"
              style={{ fontFamily: 'monospace', fontSize: 13, minHeight: 70 }}
              value={cookiesInput}
              onChange={e => setCookiesInput(e.target.value)}
              placeholder='{"SESSION": "abc123", "token": "xyz"}'
              required
            />
          </div>
          {addMsg && <div className={`form-msg form-msg--${addMsg.ok ? 'ok' : 'err'}`}>{addMsg.text}</div>}
          <button className="btn-primary" type="submit" disabled={adding}>
            {adding ? 'Adding\u2026' : 'Add Rule'}
          </button>
        </form>
      </div>
    </div>
  )
}