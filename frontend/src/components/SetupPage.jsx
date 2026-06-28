import { useState } from 'react';
import { doSetup } from '../api.js';

export default function SetupPage({ onComplete }) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [error, setError] = useState(null);
  const [loading, setLoading] = useState(false);

  async function handleSubmit(e) {
    e.preventDefault();
    if (password !== confirm) {
      setError('Passwords do not match');
      return;
    }
    if (password.length < 8) {
      setError('Password must be at least 8 characters');
      return;
    }
    setError(null);
    setLoading(true);
    try {
      await doSetup(username, password);
      onComplete();
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="setup-page">
      <div className="setup-card">
        <h1 className="setup-brand">Archivr</h1>
        <p className="setup-tagline">Create your owner account to get started.</p>
        <form onSubmit={handleSubmit}>
          <div className="setup-field">
            <label className="setup-label" htmlFor="setup-username">Username</label>
            <input
              className="setup-input"
              id="setup-username"
              type="text"
              value={username}
              onChange={e => setUsername(e.target.value)}
              autoFocus
              required
              autoComplete="username"
            />
          </div>
          <div className="setup-field">
            <label className="setup-label" htmlFor="setup-password">Password</label>
            <input
              className="setup-input"
              id="setup-password"
              type="password"
              value={password}
              onChange={e => setPassword(e.target.value)}
              required
              autoComplete="new-password"
            />
          </div>
          <div className="setup-field">
            <label className="setup-label" htmlFor="setup-confirm">Confirm password</label>
            <input
              className="setup-input"
              id="setup-confirm"
              type="password"
              value={confirm}
              onChange={e => setConfirm(e.target.value)}
              required
              autoComplete="new-password"
            />
          </div>
          {error && <p className="setup-error">{error}</p>}
          <button className="setup-submit" type="submit" disabled={loading}>
            {loading ? 'Creating account\u2026' : 'Create account'}
          </button>
        </form>
      </div>
    </div>
  );
}
