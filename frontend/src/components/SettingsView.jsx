import { useState, useEffect, useContext, useCallback } from 'react'
import { AuthContext } from '../App.jsx'
import {
  updateProfile, changePassword,
  listTokens, createToken, deleteToken,
  getInstanceSettings, updateInstanceSettings,
} from '../api.js'

const ROLE_ADMIN = 4

export default function SettingsView({ tab, onTabChange }) {
  const { currentUser, setCurrentUser } = useContext(AuthContext) ?? {}
  const isAdmin = currentUser && ((currentUser.role_bits & ROLE_ADMIN) !== 0)

  const tabs = ['profile', 'tokens', ...(isAdmin ? ['instance'] : [])]
  const tabLabels = { profile: 'Profile', tokens: 'API Tokens', instance: 'Instance' }

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
          <div className="muted">Loading\u2026</div>
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

  if (loading) return <div className="muted">Loading\u2026</div>
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
