import { useState, useEffect, useContext, useCallback } from 'react'
import { AuthContext } from '../App.jsx'
import {
  updateProfile, changePassword,
  listTokens, createToken, deleteToken,
  getInstanceSettings, updateInstanceSettings,
} from '../api.js'

const ROLE_ADMIN = 4
const ROLE_OWNER = 8

export default function SettingsView() {
  const { currentUser, setCurrentUser } = useContext(AuthContext) ?? {}
  const isAdmin = currentUser && ((currentUser.role_bits & ROLE_ADMIN) !== 0)
  const [tab, setTab] = useState('profile')

  return (
    <section className="admin-view">
      <h1>Settings</h1>
      <div className="admin-tabs" style={{ display: 'flex', gap: 12, marginBottom: 20 }}>
        {['profile', 'tokens', ...(isAdmin ? ['instance'] : [])].map(t => (
          <button key={t}
            className={`nav-link${tab === t ? ' is-active' : ''}`}
            style={{ borderBottom: tab === t ? '2px solid var(--accent)' : undefined, color: 'var(--ink)' }}
            onClick={() => setTab(t)}>
            {t === 'profile' ? 'Profile' : t === 'tokens' ? 'API Tokens' : 'Instance'}
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
    <div style={{ maxWidth: 480 }}>
      <div className="admin-section">
        <h2 style={{ fontSize: 15, marginBottom: 12 }}>Display Name</h2>
        <form className="admin-form" onSubmit={handleSaveProfile}>
          <input className="admin-input" placeholder={currentUser?.username ?? ''}
            value={displayName} onChange={e => setDisplayName(e.target.value)} />
          {saveMsg && (
            <div className={saveMsg.ok ? 'muted' : 'capture-error'}>{saveMsg.text}</div>
          )}
          <button className="capture-submit" type="submit" disabled={saving}>
            {saving ? 'Saving\u2026' : 'Save'}
          </button>
        </form>
      </div>

      <div className="admin-section" style={{ marginTop: 28 }}>
        <h2 style={{ fontSize: 15, marginBottom: 12 }}>Change Password</h2>
        <form className="admin-form" onSubmit={handleChangePassword}>
          <input className="admin-input" type="password" placeholder="Current password"
            value={curPw} onChange={e => setCurPw(e.target.value)} required />
          <input className="admin-input" type="password" placeholder="New password"
            value={newPw} onChange={e => setNewPw(e.target.value)} required />
          <input className="admin-input" type="password" placeholder="Confirm new password"
            value={confirmPw} onChange={e => setConfirmPw(e.target.value)} required />
          {pwMsg && (
            <div className={pwMsg.ok ? 'muted' : 'capture-error'}>{pwMsg.text}</div>
          )}
          <button className="capture-submit" type="submit" disabled={pwSaving}>
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
  const [newToken, setNewToken] = useState(null) // { raw_token, name }

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
    <div style={{ maxWidth: 640 }}>
      <h2 style={{ fontSize: 15, marginBottom: 12 }}>API Tokens</h2>
      {newToken && (
        <div style={{ background: '#e8f5e9', border: '1px solid #a5d6a7', padding: '12px 14px', marginBottom: 16, fontSize: 13 }}>
          <strong>Token created.</strong> Copy it now \u2014 it will not be shown again.<br />
          <code style={{ wordBreak: 'break-all', display: 'block', marginTop: 6, padding: '6px 8px', background: '#f5f5f5', border: '1px solid #ddd' }}>
            {newToken.raw_token}
          </code>
          <button style={{ marginTop: 8, fontSize: 12, border: '1px solid #aaa', background: 'none', cursor: 'pointer' }}
            onClick={() => setNewToken(null)}>Dismiss</button>
        </div>
      )}
      <form className="admin-form" onSubmit={handleCreate} style={{ display: 'flex', gap: 8, marginBottom: 16 }}>
        <input className="admin-input" placeholder="Token name" value={newName}
          onChange={e => setNewName(e.target.value)} style={{ flex: 1 }} required />
        <button className="capture-submit" type="submit" disabled={creating}>
          {creating ? 'Creating\u2026' : 'Create'}
        </button>
      </form>
      {error && <div className="capture-error">{error}</div>}
      {loading ? <div className="muted">Loading\u2026</div> : (
        <div className="admin-list">
          {tokens.length === 0 && <div className="muted">No tokens yet.</div>}
          {tokens.map(tok => (
            <div key={tok.token_uid} style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between',
              border: '1px solid var(--line)', background: 'var(--paper-3)', padding: '10px 12px' }}>
              <div>
                <strong style={{ fontSize: 14 }}>{tok.name}</strong>
                <div className="muted" style={{ fontSize: 12, marginTop: 2 }}>
                  Created {tok.created_at.slice(0, 10)}
                  {tok.last_used_at && ` \u00b7 Last used ${tok.last_used_at.slice(0, 10)}`}
                </div>
              </div>
              <button onClick={() => handleDelete(tok.token_uid)}
                style={{ border: '1px solid var(--line)', background: 'none', cursor: 'pointer',
                  color: 'var(--accent)', fontSize: 12, padding: '4px 10px' }}>
                Revoke
              </button>
            </div>
          ))}
        </div>
      )}
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
  if (error) return <div className="capture-error">{error}</div>
  if (!settings) return null

  return (
    <div style={{ maxWidth: 480 }}>
      <h2 style={{ fontSize: 15, marginBottom: 12 }}>Instance Settings</h2>
      <form onSubmit={handleSave}>
        <div className="admin-section">
          {[
            ['public_index_enabled', 'Public index (unauthenticated browsing)'],
            ['public_entry_content_enabled', 'Public entry content'],
            ['open_registration_enabled', 'Open registration'],
          ].map(([key, label]) => (
            <label key={key} style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 12, cursor: 'pointer' }}>
              <input type="checkbox" checked={!!settings[key]}
                onChange={e => setSettings(s => ({ ...s, [key]: e.target.checked }))} />
              {label}
            </label>
          ))}
          <div style={{ marginBottom: 12 }}>
            <label style={{ display: 'block', fontWeight: 600, marginBottom: 4, fontSize: 13 }}>Default entry visibility</label>
            <select className="admin-input" value={settings.default_entry_visibility}
              onChange={e => setSettings(s => ({ ...s, default_entry_visibility: Number(e.target.value) }))}>
              <option value={0}>Private (0)</option>
              <option value={2}>Unlisted (2)</option>
              <option value={3}>Public (3)</option>
            </select>
          </div>
        </div>
        {saveMsg && <div className={saveMsg.ok ? 'muted' : 'capture-error'}>{saveMsg.text}</div>}
        <button className="capture-submit" type="submit" disabled={saving}>
          {saving ? 'Saving\u2026' : 'Save Settings'}
        </button>
      </form>
    </div>
  )
}
