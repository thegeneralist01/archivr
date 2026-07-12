import { useEffect } from 'react'
import PreviewPanel from './PreviewPanel'

export default function PreviewModal({ archiveId, entry, detail, onClose }) {
  // Close on Escape key
  useEffect(() => {
    const handler = (e) => { if (e.key === 'Escape') onClose() }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [onClose])

  return (
    <div className="preview-modal-backdrop" onClick={onClose}>
      <div
        className={`preview-modal${
          detail?.summary?.entity_kind === 'tweet' ||
          detail?.summary?.entity_kind === 'tweet_thread'
            ? ''
            : ' preview-modal--full'
        }`}
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
