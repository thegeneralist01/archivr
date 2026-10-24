import { useTheme } from '../useTheme.js'

export function SunIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <circle cx="8" cy="8" r="3" />
      <line x1="8" y1="1" x2="8" y2="2.5" />
      <line x1="8" y1="13.5" x2="8" y2="15" />
      <line x1="3.05" y1="3.05" x2="4.11" y2="4.11" />
      <line x1="11.89" y1="11.89" x2="12.95" y2="12.95" />
      <line x1="1" y1="8" x2="2.5" y2="8" />
      <line x1="13.5" y1="8" x2="15" y2="8" />
      <line x1="3.05" y1="12.95" x2="4.11" y2="11.89" />
      <line x1="11.89" y1="4.11" x2="12.95" y2="3.05" />
    </svg>
  )
}

export function MoonIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M13.5 9.8A6 6 0 1 1 6.2 2.5a4.7 4.7 0 0 0 7.3 7.3z" />
    </svg>
  )
}

export default function ThemeToggle({ className = 'theme-toggle-btn' }) {
  const { preference, resolved, toggleTheme } = useTheme()
  return (
    <button
      type="button"
      className={className}
      aria-label={`Switch to ${resolved === 'dark' ? 'light' : 'dark'} mode`}
      title={`Theme: ${preference} (${resolved}). Click to toggle.`}
      onClick={toggleTheme}
    >
      {resolved === 'dark' ? <SunIcon /> : <MoonIcon />}
    </button>
  )
}
