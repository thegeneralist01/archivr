export function formatBytes(bytes) {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let size = bytes;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

export function decodeHtmlEntities(str) {
  if (!str) return str;
  return str
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&apos;/g, "'");
}

export function valueText(value) {
  return decodeHtmlEntities(value) ?? "";
}

export function formatTimestamp(value) {
  if (!value) return "";
  const d = new Date(value);
  if (isNaN(d)) return value;
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}`;
}

export const SOURCE_ICONS = {
  youtube: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path fill="#FF0000" d="M23.5 6.2a3 3 0 0 0-2.1-2.1C19.5 3.6 12 3.6 12 3.6s-7.5 0-9.4.5A3 3 0 0 0 .5 6.2C0 8.1 0 12 0 12s0 3.9.5 5.8a3 3 0 0 0 2.1 2.1c1.9.5 9.4.5 9.4.5s7.5 0 9.4-.5a3 3 0 0 0 2.1-2.1C24 15.9 24 12 24 12s0-3.9-.5-5.8z"/><polygon fill="#fff" points="9.6,15.6 15.8,12 9.6,8.4"/></svg>`,
  x: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M18.2 2h3.3l-7.2 8.2L23 22h-6.6l-5.2-6.8L5 22H1.7l7.7-8.8L1 2h6.8l4.7 6.2zm-1.1 18h1.8L6.9 3.9H5z"/></svg>`,
  instagram: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="ig" x1="0%" y1="100%" x2="100%" y2="0%"><stop offset="0%" stop-color="#f09433"/><stop offset="25%" stop-color="#e6683c"/><stop offset="50%" stop-color="#dc2743"/><stop offset="75%" stop-color="#cc2366"/><stop offset="100%" stop-color="#bc1888"/></linearGradient></defs><rect width="24" height="24" rx="5" fill="url(#ig)"/><rect x="2.5" y="2.5" width="19" height="19" rx="4" fill="none" stroke="#fff" stroke-width="1.5"/><circle cx="12" cy="12" r="4" fill="none" stroke="#fff" stroke-width="1.5"/><circle cx="17.5" cy="6.5" r="1" fill="#fff"/></svg>`,
  facebook: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><rect width="24" height="24" rx="4" fill="#1877F2"/><path fill="#fff" d="M16 8h-2c-.6 0-1 .4-1 1v2h3l-.4 3H13v8h-3v-8H8v-3h2V9a4 4 0 0 1 4-4h2v3z"/></svg>`,
  tiktok: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M19.6 5.4a4.8 4.8 0 0 1-4.8-4.8h-3v14.7a2 2 0 1 1-2-2l.1-3a5 5 0 1 0 4.9 5V8.4a8 8 0 0 0 4.8 1.6V6.7a4.8 4.8 0 0 1-0-.2l0 0 .1.9z" fill="#000"/><path d="M18.6 4.4a4.8 4.8 0 0 1-4.8-4.8h-3v14.7a2 2 0 1 1-2-2l.1-3a5 5 0 1 0 4.9 5V7.4a8 8 0 0 0 4.8 1.6V5.7" fill="none" stroke="#69C9D0" stroke-width="1.5"/></svg>`,
  reddit: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="12" fill="#FF4500"/><path fill="#fff" d="M20 12a2 2 0 0 0-2-2 2 2 0 0 0-1.3.5c-1.3-.9-3-.9-4.6-.9l.8-3.7 2.6.5a1.4 1.4 0 1 0 1.4-1.3 1.4 1.4 0 0 0-1.3.8l-2.9-.5a.3.3 0 0 0-.4.3l-.9 4.1c-1.6 0-3.1.1-4.3.9A2 2 0 0 0 4 12a2 2 0 0 0 1 1.7 3.3 3.3 0 0 0 0 .5c0 2.6 3 4.8 6.8 4.8s6.8-2.1 6.8-4.8a3.3 3.3 0 0 0 0-.5A2 2 0 0 0 20 12zm-13.6 1a1.1 1.1 0 1 1 1.1 1.1A1.1 1.1 0 0 1 6.4 13zm6.2 3.1a3.5 3.5 0 0 1-2.3.7 3.5 3.5 0 0 1-2.3-.7.3.3 0 0 1 .4-.4 3 3 0 0 0 1.9.5 3 3 0 0 0 1.9-.5.3.3 0 0 1 .4.4zm-.3-2a1.1 1.1 0 1 1 1.1-1.1A1.1 1.1 0 0 1 12.3 14.1z"/></svg>`,
  snapchat: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><rect width="24" height="24" rx="4" fill="#FFFC00"/><path d="M12 3.5c-2.4 0-4.4 2-4.4 4.4v1.3l-.8.1c-.3 0-.5.2-.5.5s.3.5.6.6c.3.1.5.3.5.8 0 .3-.1.5-.3.7-.6.8-1.5 1.3-2.2 1.4-.1.7.7 1 1.7 1.2.1.6.3.8.8.8.6 0 1.1.4 2.7.4 1.5 0 2.1-.4 2.7-.4.5 0 .7-.2.8-.8 1-.2 1.8-.5 1.7-1.2-.7-.1-1.6-.6-2.2-1.4-.2-.2-.3-.4-.3-.7 0-.5.2-.7.5-.8.3-.1.6-.3.6-.6s-.2-.5-.5-.5l-.8-.1V7.9C16.4 5.5 14.4 3.5 12 3.5z"/></svg>`,
  local: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path fill="none" stroke="#666" stroke-width="1.5" stroke-linejoin="round" d="M4 4h7l2 2h7v14H4z"/></svg>`,
  web: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="10" fill="none" stroke="#555" stroke-width="1.5"/><ellipse cx="12" cy="12" rx="4" ry="10" fill="none" stroke="#555" stroke-width="1.5"/><line x1="2" y1="9" x2="22" y2="9" stroke="#555" stroke-width="1.5"/><line x1="2" y1="15" x2="22" y2="15" stroke="#555" stroke-width="1.5"/></svg>`,
  other: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="10" fill="none" stroke="#999" stroke-width="1.5"/><text x="12" y="16" text-anchor="middle" font-size="12" fill="#999">?</text></svg>`,
};

export function sourceIconSvg(kind) {
  return SOURCE_ICONS[kind] ?? SOURCE_ICONS.other;
}

// Humanize each slash-segment of a stored full_path for display.
// Matches the backend humanize_slug convention: capitalize first char of each
// hyphen-part, replace hyphens with spaces.
// e.g. "/x/natural-science" → "/X/Natural Science"
// Used only for rendered label text; never mutate state/API values with this.
export function displayPath(fullPath) {
  return fullPath
    .split('/')
    .map(seg =>
      seg
        ? seg.split('-').map(p => p.charAt(0).toUpperCase() + p.slice(1)).join(' ')
        : ''
    )
    .join('/');
}
