import { useEffect } from 'react'
import PreviewPanel from './PreviewPanel'

export default function PreviewModal({ archiveId, entry, detail, onClose }) {
  const entityKind = detail?.summary?.entity_kind
  // A focused post uses Escape to return to the conversation first.
  useEffect(() => {
    const handler = (e) => {
      if (e.key !== 'Escape') return
      if (entityKind === 'tweet_thread' && /^#tweet-\d+$/.test(window.location.hash)
        && !document.querySelector('.tw-media-lightbox')) {
        e.preventDefault()
        window.history.replaceState(window.history.state, '', window.location.pathname + window.location.search)
        window.dispatchEvent(new Event('hashchange'))
        return
      }
      onClose()
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [entityKind, onClose])

  return (
    <div className="preview-modal-backdrop" onClick={onClose}>
      <div
        className={`preview-modal${
          entityKind === 'tweet' || entityKind === 'tweet_thread' ? '' : ' preview-modal--full'
        }${entityKind === 'tweet_thread' ? ' preview-modal--thread' : ''}`}
        onClick={e => e.stopPropagation()}
      >
        <div className="preview-modal-header">
          <span className="preview-modal-title">{entry?.title || entry?.entry_uid || 'Preview'}</span>
          <a
            className="preview-modal-newtab"
            href={`/preview/${archiveId}/${entry?.entry_uid}`}
            target="_blank"
            rel="noopener noreferrer"
            title="Open in new tab"
          >↗</a>
          <button className="preview-modal-close" onClick={onClose} aria-label="Close preview">×</button>
        </div>
        <div className="preview-modal-body">
          <PreviewPanel archiveId={archiveId} entry={entry} detail={detail} />
        </div>
      </div>
    </div>
  )
}
