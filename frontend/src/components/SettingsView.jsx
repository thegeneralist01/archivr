import { useState, useEffect, useContext, useCallback, useRef } from 'react'
import { AuthContext } from '../App.jsx'
import {
  updateProfile, changePassword, patchMe,
  listTokens, createToken, deleteToken,
  listSessions, revokeSession, revokeOtherSessions,
  getInstanceSettings, updateInstanceSettings,
  scanOrphanBlobs, deleteOrphanBlobs,
  listCookieRules, createCookieRule, updateCookieRule, deleteCookieRule,
  listRoles, fetchMe,
  getYtDlpStatus, updateYtDlp,
} from '../api.js'
import { describeExpiry, formatTimestamp } from '../utils.js'

const ROLE_ADMIN = 4
const ROLE_OWNER = 8

export default function SettingsView({ tab, onTabChange, archiveId }) {
  const { currentUser, setCurrentUser } = useContext(AuthContext) ?? {}
  const isAdmin = currentUser && ((currentUser.role_bits & ROLE_ADMIN) !== 0)
  const isOwner = !!currentUser && (currentUser.role_bits & ROLE_OWNER) !== 0

  const tabs = ['profile', 'tokens', 'sessions', ...(isAdmin ? ['instance', 'cookies', 'extensions', 'storage'] : [])]
  const tabLabels = { profile: 'Profile', tokens: 'API Tokens', sessions: 'Sessions', instance: 'Instance', cookies: 'Cookies', extensions: 'Extensions', storage: 'Storage' }

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
      {tab === 'sessions' && <SessionsTab />}
      {tab === 'instance' && isAdmin && (
        <>
          <InstanceTab isOwner={isOwner} setCurrentUser={setCurrentUser} />
          <YtDlpSection />
        </>
      )}
      {tab === 'cookies' && isAdmin && <CookiesTab />}
      {tab === 'extensions' && isAdmin && <ExtensionsTab />}
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
  const [newScope, setNewScope] = useState('full')
  const [newExpiry, setNewExpiry] = useState('')
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
      const tok = await createToken(newName.trim(), {
        expiresInDays: newExpiry ? Number(newExpiry) : null,
        scope: newScope,
      })
      setNewToken(tok)
      setNewName('')
      setNewScope('full')
      setNewExpiry('')
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
          <select className="field-input" aria-label="Token scope"
            value={newScope} onChange={e => setNewScope(e.target.value)}>
            <option value="full">Full access</option>
            <option value="read">Read-only</option>
          </select>
          <select className="field-input" aria-label="Token expiry"
            value={newExpiry} onChange={e => setNewExpiry(e.target.value)}>
            <option value="">Never</option>
            <option value="30">30 days</option>
            <option value="90">90 days</option>
            <option value="365">1 year</option>
          </select>
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
            {tokens.map(tok => {
              const expiry = describeExpiry(tok.expires_at)
              return (
              <div key={tok.token_uid} className="token-row">
                <div className="token-row-info">
                  <div className="token-row-title">
                    <strong>{tok.name}</strong>
                    {tok.scope === 'read' && <span className="token-badge">read-only</span>}
                  </div>
                  <div className="muted">
                    Created {tok.created_at.slice(0, 10)}
                    {tok.last_used_at && ` \u00b7 Last used ${tok.last_used_at.slice(0, 10)}`}
                  </div>
                  <div className={`token-expiry${expiry.expired ? ' token-expiry--expired' : ''}`}>
                    {expiry.text}
                  </div>
                </div>
                <button className="btn-danger" style={{ fontSize: 12, padding: '4px 10px' }}
                  onClick={() => handleDelete(tok.token_uid)}>
                  Revoke
                </button>
              </div>
              )
            })}
          </div>
        )}
      </div>
    </div>
  )
}

function SessionsTab() {
  const [sessions, setSessions] = useState([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)
  const [busy, setBusy] = useState(false)

  const refresh = useCallback(async () => {
    setLoading(true); setError(null)
    try { setSessions(await listSessions()) }
    catch (e) { setError(e.message) }
    finally { setLoading(false) }
  }, [])

  useEffect(() => { refresh() }, [refresh])

  const others = sessions.filter(s => !s.current)

  async function handleRevoke(session) {
    if (!window.confirm('Sign out this device?')) return
    try {
      await revokeSession(session.session_handle)
      setSessions(ss => ss.filter(s => s.session_handle !== session.session_handle))
    } catch (e) {
      setError(e.message)
    }
  }

  async function handleRevokeOthers() {
    if (!window.confirm('Sign out all other devices? Your current session stays signed in.')) return
    setBusy(true); setError(null)
    try {
      await revokeOtherSessions()
      setSessions(ss => ss.filter(s => s.current))
    } catch (e) {
      setError(e.message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div style={{ maxWidth: 600 }}>
      <div className="form-section">
        <h2>Sessions</h2>
        <p className="muted">Devices currently signed in to your account.</p>
        <button className="btn-danger" type="button" style={{ fontSize: 12, padding: '4px 10px' }}
          disabled={busy || loading || others.length === 0}
          onClick={handleRevokeOthers}>
          Sign out other devices
        </button>
        {error && <div className="form-msg form-msg--err">{error}</div>}
        {loading ? (
          <div className="muted">Loading…</div>
        ) : (
          <div>
            {sessions.length === 0 && <div className="muted">No active sessions.</div>}
            {sessions.map(s => (
              <div key={s.session_handle} className="session-row">
                <div className="token-row-info">
                  <div className="token-row-title">
                    <strong>{s.user_agent || 'Unknown device'}</strong>
                    {s.current && <span className="session-badge">This device</span>}
                  </div>
                  <div className="muted">Signed in {formatTimestamp(s.created_at)}</div>
                  <div className="muted">Last seen {formatTimestamp(s.last_seen_at)}</div>
                  <div className="muted">Expires {formatTimestamp(s.expires_at)}</div>
                </div>
                {!s.current && (
                  <button className="btn-danger" style={{ fontSize: 12, padding: '4px 10px' }}
                    onClick={() => handleRevoke(s)}>
                    Revoke
                  </button>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

const TITLE_MODEL_PROVIDERS = [
  ['anthropic_http', 'Anthropic API'],
  ['openai_compatible', 'OpenAI-compatible API'],
  ['claude_cli', 'Claude CLI'],
  ['codex_cli', 'Codex CLI'],
]

function InstanceTab({ isOwner, setCurrentUser }) {
  const [settings, setSettings] = useState(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)
  const [saving, setSaving] = useState(false)
  const [saveMsg, setSaveMsg] = useState(null)
  const [roles, setRoles] = useState([])
  const [reorderBits, setReorderBits] = useState(12)
  const [permSaving, setPermSaving] = useState(false)
  const [permMsg, setPermMsg] = useState(null)

  useEffect(() => {
    (async () => {
      try {
        const [s, r] = await Promise.all([getInstanceSettings(), listRoles()])
        setSettings(s)
        setRoles(r.filter(role => role.bit_position > 0))
        setReorderBits(s.reorder_children_role_bits ?? 12)
      }
      catch (e) { setError(e.message) }
      finally { setLoading(false) }
    })()
  }, [])

  function toggleRole(bit, checked) {
    setReorderBits(b => checked ? (b | bit) >>> 0 : (b & ~bit) >>> 0)
  }

  async function handleSavePermissions(e) {
    e.preventDefault()
    setPermSaving(true); setPermMsg(null)
    try {
      await updateInstanceSettings({ reorder_children_role_bits: reorderBits })
      setSettings(s => ({ ...s, reorder_children_role_bits: reorderBits }))
      const me = await fetchMe()
      if (me) setCurrentUser?.(me) // owner's own can_reorder_children may change
      setPermMsg({ ok: true, text: 'Saved.' })
    } catch (err) {
      setPermMsg({ ok: false, text: err.message })
    } finally {
      setPermSaving(false)
    }
  }

  async function handleSave(e) {
    e.preventDefault()
    setSaving(true); setSaveMsg(null)
    try {
      const { reorder_children_role_bits: _mask, title_models: _models, ...rest } = settings
      await updateInstanceSettings(rest)
      setSettings(await getInstanceSettings()) // server-trimmed models + effective sources
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
          <div className="form-field" style={{ marginTop: 4 }}>
            <label className="form-label">Thread title models</label>
            {TITLE_MODEL_PROVIDERS.map(([kind, label]) => {
              const key = `title_model_${kind}`
              const info = settings.title_models?.[kind]
              const placeholder = info ? `${info.fallback_model} (${info.fallback_source})` : ''
              return (
                <div key={kind} className="form-field">
                  <label className="form-label" htmlFor={key}>{label}</label>
                  <input id={key} className="field-input" type="text" maxLength={100}
                    value={settings[key] ?? ''} placeholder={placeholder}
                    onChange={e => setSettings(s => ({ ...s, [key]: e.target.value }))} />
                </div>
              )
            })}
            <p className="form-hint">Cheap model used for Generate title. Leave blank to use the default.</p>
          </div>
          {saveMsg && <div className={`form-msg form-msg--${saveMsg.ok ? 'ok' : 'err'}`}>{saveMsg.text}</div>}
          <button className="btn-primary" type="submit" disabled={saving}>
            {saving ? 'Saving\u2026' : 'Save Settings'}
          </button>
        </form>
      </div>
      <div className="form-section">
        <h2>Permissions</h2>
        <form onSubmit={handleSavePermissions}>
          <label className="form-label">Reorder child entries</label>
          {roles.map(role => {
            const bit = (1 << role.bit_position) >>> 0
            return (
              <label key={role.role_uid} className="checkbox-row">
                <input type="checkbox" disabled={!isOwner || permSaving}
                  checked={(reorderBits & bit) !== 0}
                  onChange={e => toggleRole(bit, e.target.checked)} />
                {role.name}{!role.is_builtin && <span className="muted"> (custom)</span>}
              </label>
            )
          })}
          <p className="form-hint">Roles are cumulative: every signed-in account also has User, and owners also have Admin. Checking User lets every signed-in account reorder.</p>
          {!isOwner && <p className="form-hint">Only the owner can change this.</p>}
          {permMsg && <div className={`form-msg form-msg--${permMsg.ok ? 'ok' : 'err'}`}>{permMsg.text}</div>}
          {isOwner && (
            <button className="btn-primary" type="submit" disabled={permSaving}>
              {permSaving ? 'Saving\u2026' : 'Save Permissions'}
            </button>
          )}
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

function ExtensionsTab() {
  const [settings, setSettings] = useState(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [msg, setMsg] = useState(null)

  useEffect(() => {
    (async () => {
      try { setSettings(await getInstanceSettings()) }
      catch (e) { setMsg({ ok: false, text: e.message }) }
      finally { setLoading(false) }
    })()
  }, [])

  async function toggleUblock(val) {
    setSaving(true)
    setMsg(null)
    try {
      await updateInstanceSettings({ ublock_enabled: val })
      setSettings(s => ({ ...s, ublock_enabled: val }))
      setMsg({ ok: true, text: 'Saved.' })
    } catch (e) {
      setMsg({ ok: false, text: e.message })
    } finally {
      setSaving(false)
    }
  }

  async function toggleCookieExt(val) {
    setSaving(true)
    setMsg(null)
    try {
      await updateInstanceSettings({ cookie_ext_enabled: val })
      setSettings(s => ({ ...s, cookie_ext_enabled: val }))
      setMsg({ ok: true, text: 'Saved.' })
    } catch (e) {
      setMsg({ ok: false, text: e.message })
    } finally {
      setSaving(false)
    }
  }

  async function toggleModalCloser(val) {
    setSaving(true)
    setMsg(null)
    try {
      await updateInstanceSettings({ modal_closer_enabled: val })
      setSettings(s => ({ ...s, modal_closer_enabled: val }))
      setMsg({ ok: true, text: 'Saved.' })
    } catch (e) {
      setMsg({ ok: false, text: e.message })
    } finally {
      setSaving(false)
    }
  }

  if (loading) return <div className="muted">Loading\u2026</div>

  const extAvailable = settings?.ublock_ext_available ?? false
  const extEnabled = settings?.ublock_enabled ?? true
  const cookieExtAvailable = settings?.cookie_ext_available ?? false
  const cookieExtEnabled = settings?.cookie_ext_enabled ?? true
  const modalCloserEnabled = settings?.modal_closer_enabled ?? true

  return (
    <div>
      <div className="form-section">
        <h2>Extensions</h2>
        <p className="form-hint" style={{ marginBottom: 20 }}>
          Extensions run inside the browser during WebPage captures and can block ads,
          accept cookie banners, and more. Changes take effect on the next capture.
        </p>

        <div className="ext-grid">
          <div className="ext-card">
            <div className="ext-card-header">
              <div className="ext-card-info">
                <span className="ext-card-name">uBlock Origin Lite</span>
                <span className="ext-card-desc">
                  Blocks ads, trackers, and other page clutter during archiving
                  via Chrome&rsquo;s declarativeNetRequest API (Manifest V3).
                </span>
                {!extAvailable && (
                  <span className="ext-card-hint">
                    Not configured &mdash; set <code>ARCHIVR_UBLOCK_EXT</code> to the
                    unpacked extension directory to enable.
                  </span>
                )}
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={extEnabled}
                className={`ext-toggle${extEnabled ? ' ext-toggle--on' : ''}`}
                onClick={() => toggleUblock(!extEnabled)}
                disabled={saving}
                aria-label="Toggle uBlock Origin Lite"
              >
                <span className="ext-toggle-knob" />
              </button>
            </div>
          </div>

          <div className="ext-card">
            <div className="ext-card-header">
              <div className="ext-card-info">
                <span className="ext-card-name">I Still Don&rsquo;t Care About Cookies</span>
                <span className="ext-card-desc">
                  Dismiss cookie consent banners during archiving.
                </span>
                {!cookieExtAvailable && (
                  <span className="ext-card-hint">
                    Not configured &mdash; set <code>ARCHIVR_COOKIE_EXT</code> to the
                    unpacked extension directory to enable.
                  </span>
                )}
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={cookieExtEnabled}
                className={`ext-toggle${cookieExtEnabled ? ' ext-toggle--on' : ''}`}
                onClick={() => toggleCookieExt(!cookieExtEnabled)}
                disabled={saving}
                aria-label="Toggle I Still Don't Care About Cookies"
              >
                <span className="ext-toggle-knob" />
              </button>
            </div>
          </div>

          <div className="ext-card">
            <div className="ext-card-header">
              <div className="ext-card-info">
                <span className="ext-card-name">Modal &amp; Dialog Closer</span>
                <span className="ext-card-desc">
                  Auto-dismiss cookie banners, consent overlays, and other modal dialogs
                  before a WebPage capture is taken. Implemented as an injected browser
                  script; no external extension required.
                </span>
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={modalCloserEnabled}
                className={`ext-toggle${modalCloserEnabled ? ' ext-toggle--on' : ''}`}
                onClick={() => toggleModalCloser(!modalCloserEnabled)}
                disabled={saving}
                aria-label="Toggle Modal and Dialog Closer"
              >
                <span className="ext-toggle-knob" />
              </button>
            </div>
          </div>
        </div>

        {msg && <div className={`form-msg form-msg--${msg.ok ? 'ok' : 'err'}`}>{msg.text}</div>}
      </div>
    </div>
  )
}

const YT_DLP_SOURCE_LABELS = {
  force: 'Forced (ARCHIVR_YT_DLP_FORCE)',
  env: 'Pinned (ARCHIVR_YT_DLP)',
  'state-dir': 'Managed install',
  path: 'System PATH',
}
const JS_SOURCE_LABELS = {
  force: 'Forced (ARCHIVR_JS_RUNTIME)',
  env: 'Pinned (ARCHIVR_DENO)',
  'state-dir': 'Managed install',
  path: 'System PATH',
}

function sourceLabel(labels, row) {
  return (row?.role && labels[row.role]) || row?.label || 'unknown source'
}

function YtDlpCandidateTable({ caption, rows, labels }) {
  return (
    <div className="form-field">
      <div className="form-label">{caption}</div>
      <div className="ytdlp-table-wrap">
        <table className="admin-table ytdlp-table">
          <thead>
            <tr><th>Source</th><th>Path</th><th>Version</th><th></th></tr>
          </thead>
          <tbody>
            {rows.map(row => (
              <tr key={row.role} className={row.chosen ? 'ytdlp-row--chosen' : undefined}>
                <td>{sourceLabel(labels, row)}</td>
                <td className="ytdlp-path">
                  {row.path ?? <span className="muted">not set</span>}
                </td>
                <td>
                  {row.invalid
                    ? <span className="form-msg--err">invalid: {row.invalid}</span>
                    : (row.path ? (row.version ?? '\u2014') : '\u2014')}
                </td>
                <td>{row.chosen && <span className="ytdlp-badge">in use</span>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

function YtDlpSection() {
  const [status, setStatus] = useState(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)
  const [updating, setUpdating] = useState(false)
  const [result, setResult] = useState(null)
  const [updateError, setUpdateError] = useState(null)

  const loadCtrl = useRef(null)
  const alive = useRef(true)

  // Probing every candidate takes seconds, so this loads separately from the settings form.
  // A newer load or unmounting aborts the previous request. Resolves to the status or null.
  async function load() {
    loadCtrl.current?.abort()
    const ctrl = new AbortController()
    loadCtrl.current = ctrl
    setLoading(true)
    setError(null)
    try {
      const s = await getYtDlpStatus({ signal: ctrl.signal })
      if (!ctrl.signal.aborted) setStatus(s)
      return s
    } catch (e) {
      if (!ctrl.signal.aborted) setError(e.message)
      return null
    } finally {
      if (!ctrl.signal.aborted) setLoading(false)
    }
  }

  useEffect(() => {
    alive.current = true
    load()
    return () => {
      alive.current = false
      loadCtrl.current?.abort()
    }
  }, [])

  async function handleUpdate() {
    setUpdating(true)
    setResult(null)
    setUpdateError(null)
    try {
      const r = await updateYtDlp()
      setResult(r)
      setStatus(r.status)
    } catch (err) {
      // A proxy may time out (e.g. 504) while the update keeps running server-side.
      const s = err.status !== 409 && alive.current ? await load() : null
      setUpdateError(s?.update_running
        ? 'Update still running on the server — refresh in a minute.'
        : err.message)
    } finally {
      setUpdating(false)
    }
  }

  const ytChosen = status?.yt_dlp_chosen
  const ytChosenRow = (status?.yt_dlp ?? []).find(r => r.chosen)
  const jsChosen = status?.js_runtime_chosen
  const inUse = status?.js_runtime_in_use
  const invalidRows = (status?.js_runtime ?? []).filter(r => r.invalid)

  return (
    <div className="form-section ytdlp-section">
      <h2>yt-dlp</h2>
      {loading && !status && <div className="muted">Loading{'\u2026'}</div>}
      {error && <div className="form-msg form-msg--err">{error}</div>}

      {status && (
        <>
          <dl className="ytdlp-summary">
            <dt>yt-dlp</dt>
            <dd>
              {ytChosen?.version ?? 'unknown'}
              <span className="muted"> · {ytChosen?.role ? YT_DLP_SOURCE_LABELS[ytChosen.role] : 'unlisted path'}</span>
              {ytChosen?.path && <div className="ytdlp-path">{ytChosen.path}</div>}
            </dd>
            <dt>JS runtime</dt>
            <dd>
              {jsChosen ? (
                <>
                  {jsChosen.kind} {jsChosen.version ?? ''}
                  <span className="muted"> · {JS_SOURCE_LABELS[jsChosen.role] ?? jsChosen.role}</span>
                  {jsChosen.path && <div className="ytdlp-path">{jsChosen.path}</div>}
                </>
              ) : <span className="form-msg--err">none</span>}
            </dd>
            <dt>In use by this server</dt>
            <dd>
              {inUse ? (
                <>
                  {inUse.kind}{jsChosen && jsChosen.path === inUse.path && jsChosen.version ? ` ${jsChosen.version}` : ''}
                  {inUse.path && <div className="ytdlp-path">{inUse.path}</div>}
                </>
              ) : <span className="muted">no JS runtime</span>}
            </dd>
          </dl>

          {ytChosenRow?.invalid && (
            <div className="form-msg form-msg--err">
              The yt-dlp in use does not run: {ytChosenRow.invalid}
            </div>
          )}
          {!jsChosen && (
            <div className="form-msg form-msg--err">
              No JS runtime resolved — YouTube downloads may fail with HTTP 403. Update below to install Deno.
            </div>
          )}
          {invalidRows.map(r => (
            <div key={r.role} className="form-msg form-msg--err">
              ARCHIVR_JS_RUNTIME={r.path} is invalid ({r.invalid}) and is ignored.
            </div>
          ))}

          {status.state_dir && <p className="form-hint">State directory: <span className="ytdlp-path">{status.state_dir}</span></p>}
          {!status.yt_dlp_installed && status.yt_dlp_target && (
            <p className="form-hint">No managed yt-dlp yet — the update installs it to {status.yt_dlp_target}.</p>
          )}
          {!status.deno_installed && status.deno_target && (
            <p className="form-hint">No managed Deno yet — the update installs it to {status.deno_target}.</p>
          )}

          <YtDlpCandidateTable caption="yt-dlp candidates" rows={status.yt_dlp} labels={YT_DLP_SOURCE_LABELS} />
          <YtDlpCandidateTable caption="JS runtime candidates" rows={status.js_runtime} labels={JS_SOURCE_LABELS} />
        </>
      )}

      {(status || !loading) && (
        <div className="ytdlp-actions">
          <button className="btn-primary" type="button"
            disabled={updating || status?.update_running} onClick={handleUpdate}>
            {updating ? 'Updating\u2026 (can take a few minutes)' : 'Update yt-dlp & Deno'}
          </button>
          <button className="btn-ghost" type="button" disabled={updating || loading} onClick={load}>
            Refresh
          </button>
          {status?.update_running && !updating && <span className="form-hint">An update is already running.</span>}
        </div>
      )}

      {result && (
        <>
          {[['yt-dlp', result.yt_dlp], ['Deno', result.deno]].map(([name, o]) => (
            <div key={name} className={`form-msg form-msg--${o.ok ? 'ok' : 'err'}`}>
              {name}: {o.ok ? o.message : `failed — ${o.message}`}
            </div>
          ))}
          {(result.yt_dlp.ok || result.deno.ok) && (
            <p className="form-hint">New binaries are used for the next capture — no restart needed.</p>
          )}
        </>
      )}
      {updateError && <div className="form-msg form-msg--err">{updateError}</div>}
    </div>
  )
}