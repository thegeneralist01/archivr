import { useState, useEffect, useContext, useCallback } from 'react'
import { AuthContext } from '../App.jsx'
import {
  listAdminUsers, createAdminUser, setUserStatus, assignRole, removeRole,
  listRoles, createRole
} from '../api.js'

const ROLE_ADMIN = 4

export default function AdminView({ archives }) {
  const { currentUser } = useContext(AuthContext) ?? {}
  const isAdmin = currentUser && (currentUser.role_bits & ROLE_ADMIN) !== 0

  const [tab, setTab] = useState('users')
  const [users, setUsers] = useState([])
  const [roles, setRoles] = useState([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState(null)

  // Create user form state
  const [newUsername, setNewUsername] = useState('')
  const [newPassword, setNewPassword] = useState('')
  const [newEmail, setNewEmail] = useState('')
  const [createError, setCreateError] = useState(null)
  const [creating, setCreating] = useState(false)

  // Create role form state
  const [newRoleSlug, setNewRoleSlug] = useState('')
  const [newRoleName, setNewRoleName] = useState('')
  const [roleCreateError, setRoleCreateError] = useState(null)
  const [creatingRole, setCreatingRole] = useState(false)

  const refresh = useCallback(async () => {
    if (!isAdmin) return
    setLoading(true)
    setError(null)
    try {
      const [u, r] = await Promise.all([listAdminUsers(), listRoles()])
      setUsers(u)
      setRoles(r)
    } catch (e) {
      setError(e.message)
    } finally {
      setLoading(false)
    }
  }, [isAdmin])

  useEffect(() => { refresh() }, [refresh])

  async function handleToggleStatus(user) {
    const next = user.status === 'active' ? 'disabled' : 'active'
    try {
      await setUserStatus(user.user_uid, next)
      setUsers(us => us.map(u => u.user_uid === user.user_uid ? { ...u, status: next } : u))
    } catch (e) {
      setError(e.message)
    }
  }

  async function handleCreateUser(e) {
    e.preventDefault()
    if (!newUsername.trim() || !newPassword) { setCreateError('Username and password required'); return }
    setCreating(true)
    setCreateError(null)
    try {
      await createAdminUser(newUsername.trim(), newPassword, newEmail.trim() || undefined)
      setNewUsername('')
      setNewPassword('')
      setNewEmail('')
      await refresh()
    } catch (e) {
      setCreateError(e.message)
    } finally {
      setCreating(false)
    }
  }

  async function handleCreateRole(e) {
    e.preventDefault()
    if (!newRoleSlug.trim() || !newRoleName.trim()) { setRoleCreateError('Slug and name required'); return }
    setCreatingRole(true)
    setRoleCreateError(null)
    try {
      await createRole(newRoleSlug.trim(), newRoleName.trim())
      setNewRoleSlug('')
      setNewRoleName('')
      await refresh()
    } catch (e) {
      setRoleCreateError(e.message)
    } finally {
      setCreatingRole(false)
    }
  }

  if (!isAdmin) {
    return (
      <section id="admin-view" className="view admin-view is-active">
        <h1>Admin</h1>
        <p className="muted">You need admin privileges to access this panel.</p>
        <h2>Mounted Archives</h2>
        <div className="admin-list">
          {archives.map(a => (
            <div key={a.id} className="admin-archive">
              <strong>{a.label}</strong>
              <div className="muted">{a.archive_path}</div>
            </div>
          ))}
        </div>
      </section>
    )
  }

  return (
    <section id="admin-view" className="view admin-view is-active">
      <h1>Admin</h1>

      <div className="view-tabs">
        <button className={`view-tab${tab === 'users' ? ' is-active' : ''}`} onClick={() => setTab('users')}>Users</button>
        <button className={`view-tab${tab === 'roles' ? ' is-active' : ''}`} onClick={() => setTab('roles')}>Roles</button>
        <button className={`view-tab${tab === 'archives' ? ' is-active' : ''}`} onClick={() => setTab('archives')}>Archives</button>
      </div>

      {error && <div className="form-msg form-msg--err">{error}</div>}

      {tab === 'users' && (
        <div className="admin-section">
          <h2>Users</h2>
          {loading ? <p className="muted">Loading…</p> : (
            <table className="admin-table">
              <thead>
                <tr><th>Username</th><th>Email</th><th>Roles</th><th>Status</th><th>Actions</th></tr>
              </thead>
              <tbody>
                {users.map(u => (
                  <tr key={u.user_uid} className={u.status === 'disabled' ? 'admin-row-disabled' : ''}>
                    <td>{u.username}</td>
                    <td className="muted">{u.email || '—'}</td>
                    <td>{u.role_slugs.join(', ') || '—'}</td>
                    <td><span className={`status-badge status-${u.status}`}>{u.status}</span></td>
                    <td>
                      <button className="admin-action-btn" onClick={() => handleToggleStatus(u)}>
                        {u.status === 'active' ? 'Ban' : 'Unban'}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          <h3>Create User</h3>
          <form className="admin-form" onSubmit={handleCreateUser}>
            <input className="admin-input" placeholder="Username" value={newUsername}
              onChange={e => setNewUsername(e.target.value)} required />
            <input className="admin-input" type="password" placeholder="Password (min 8 chars)"
              value={newPassword} onChange={e => setNewPassword(e.target.value)} required />
            <input className="admin-input" type="email" placeholder="Email (optional)"
              value={newEmail} onChange={e => setNewEmail(e.target.value)} />
            {createError && <div className="form-msg form-msg--err">{createError}</div>}
            <button className="btn-primary" type="submit" disabled={creating}>
              {creating ? 'Creating\u2026' : 'Create User'}
            </button>
          </form>
        </div>
      )}

      {tab === 'roles' && (
        <div className="admin-section">
          <h2>Roles</h2>
          {loading ? <p className="muted">Loading…</p> : (
            <table className="admin-table">
              <thead>
                <tr><th>Slug</th><th>Name</th><th>Level</th><th>Bit</th><th>Built-in</th></tr>
              </thead>
              <tbody>
                {roles.map(r => (
                  <tr key={r.role_uid}>
                    <td><code>{r.slug}</code></td>
                    <td>{r.name}</td>
                    <td>{r.level}</td>
                    <td>{r.bit_position}</td>
                    <td>{r.is_builtin ? '✓' : ''}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          <h3>Create Custom Role</h3>
          <form className="admin-form" onSubmit={handleCreateRole}>
            <input className="admin-input" placeholder="Slug (e.g. moderator)" value={newRoleSlug}
              onChange={e => setNewRoleSlug(e.target.value)} required />
            <input className="admin-input" placeholder="Display Name (e.g. Moderator)"
              value={newRoleName} onChange={e => setNewRoleName(e.target.value)} required />
            {roleCreateError && <div className="form-msg form-msg--err">{roleCreateError}</div>}
            <button className="btn-primary" type="submit" disabled={creatingRole}>
              {creatingRole ? 'Creating\u2026' : 'Create Role'}
            </button>
          </form>
        </div>
      )}

      {tab === 'archives' && (
        <div className="admin-section">
          <h2>Mounted Archives</h2>
          <div className="admin-list">
            {archives.map(a => (
              <div key={a.id} className="admin-archive">
                <strong>{a.label}</strong>
                <div className="muted">{a.archive_path}</div>
              </div>
            ))}
          </div>
        </div>
      )}
    </section>
  )
}
