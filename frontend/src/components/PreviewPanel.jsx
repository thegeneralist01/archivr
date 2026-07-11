import { useEffect, useMemo } from 'react';
import VideoPreview from './VideoPreview';
import IframePreview from './IframePreview';
import ImagePreview from './ImagePreview';
import TweetPreview from './TweetPreview';

const VIDEO_EXTS = new Set(['mp4', 'webm', 'mov', 'mkv', 'avi', 'm4v', 'ogv']);
const AUDIO_EXTS = new Set(['mp3', 'ogg', 'm4a', 'opus', 'wav', 'flac', 'aac']);
const IMAGE_EXTS = new Set(['jpg', 'jpeg', 'png', 'gif', 'webp', 'avif', 'svg', 'bmp']);

export default function PreviewPanel({ archiveId, entry, detail, onSetAudio }) {
  // Compute audio URL upfront so useEffect can depend on it
  const audioInfo = useMemo(() => {
    if (!detail || !entry) return null;
    const { artifacts, summary } = detail;
    const idx = artifacts.findIndex(a => a.artifact_role === 'primary_media');
    if (idx === -1) return null;
    const ext = artifacts[idx].relpath.split('.').pop().toLowerCase();
    if (!AUDIO_EXTS.has(ext)) return null;
    return { entry, src: `/api/archives/${archiveId}/entries/${summary.entry_uid}/artifacts/${idx}`, archiveId };
  }, [detail, entry, archiveId]);

  useEffect(() => {
    if (audioInfo && onSetAudio) onSetAudio(audioInfo);
  }, [audioInfo]);

  if (!entry) {
    return (
      <div className="preview-panel preview-panel--empty">
        <span style={{ color: 'var(--muted)', fontFamily: 'var(--sans)', fontSize: '0.9rem' }}>
          Select an entry to preview
        </span>
      </div>
    );
  }

  if (!detail) {
    return (
      <div className="preview-panel preview-panel--loading">
        <span style={{ color: 'var(--muted)', fontFamily: 'var(--sans)', fontSize: '0.9rem' }}>
          Loading…
        </span>
      </div>
    );
  }

  const { summary, artifacts } = detail;
  const entryUid = summary.entry_uid;
  const entityKind = summary.entity_kind;

  // 1. Tweet / tweet thread
  if (entityKind === 'tweet' || entityKind === 'tweet_thread') {
    return (
      <div className="preview-panel">
        <TweetPreview
          archiveId={archiveId}
          entryUid={entryUid}
          artifacts={artifacts}
          entityKind={entityKind}
        />
      </div>
    );
  }

  // 2. Find primary_media artifact
  const primaryMediaIndex = artifacts.findIndex(a => a.artifact_role === 'primary_media');
  if (primaryMediaIndex === -1) {
    return (
      <div className="preview-panel preview-panel--no-preview">
        <span style={{ color: 'var(--muted)', fontFamily: 'var(--sans)', fontSize: '0.9rem' }}>
          No preview available
        </span>
        {artifacts.length > 0 && (
          <ul
            style={{
              marginTop: '12px',
              paddingLeft: '20px',
              fontSize: '0.8rem',
              color: 'var(--muted-2)',
              fontFamily: 'var(--sans)',
            }}
          >
            {artifacts.map((a, i) => (
              <li key={i}>{a.artifact_role}: {a.relpath}</li>
            ))}
          </ul>
        )}
      </div>
    );
  }

  const primaryArtifact = artifacts[primaryMediaIndex];
  const primaryMediaUrl = `/api/archives/${archiveId}/entries/${entryUid}/artifacts/${primaryMediaIndex}`;
  const ext = primaryArtifact.relpath.split('.').pop().toLowerCase();

  // 3. Video
  if (VIDEO_EXTS.has(ext)) {
    return (
      <div className="preview-panel">
        <VideoPreview src={primaryMediaUrl} />
      </div>
    );
  }

  // 4. Audio — set audio bar and also show inline player + message
  if (AUDIO_EXTS.has(ext)) {
    return (
      <div
        className="preview-panel preview-panel--audio"
        style={{
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
          gap: '12px',
          padding: '24px',
          fontFamily: 'var(--sans)',
        }}
      >
        <span style={{ fontSize: '2.5rem' }}>🎵</span>
        <span style={{ color: 'var(--ink)', fontSize: '1rem', fontWeight: 600 }}>
          {summary.title || entryUid}
        </span>
        <span style={{ color: 'var(--muted)', fontSize: '0.85rem' }}>
          Playing in audio bar below
        </span>
        <audio
          src={primaryMediaUrl}
          controls
          style={{ marginTop: '12px', width: '100%', maxWidth: '400px' }}
        />
      </div>
    );
  }

  // 5. PDF
  if (ext === 'pdf') {
    return (
      <div className="preview-panel" style={{ height: '100%' }}>
        <IframePreview src={primaryMediaUrl} type="pdf" />
      </div>
    );
  }

  // 6. HTML page
  if (ext === 'html' || ext === 'htm') {
    return (
      <div className="preview-panel" style={{ height: '100%' }}>
        <IframePreview src={primaryMediaUrl} type="page" />
      </div>
    );
  }

  // 7. Image
  if (IMAGE_EXTS.has(ext)) {
    return (
      <div className="preview-panel" style={{ height: '100%' }}>
        <ImagePreview src={primaryMediaUrl} alt={summary.title || 'Image'} />
      </div>
    );
  }

  // 8. Fallback
  return (
    <div className="preview-panel preview-panel--no-preview">
      <span style={{ color: 'var(--muted)', fontFamily: 'var(--sans)', fontSize: '0.9rem' }}>
        No preview available
      </span>
      <ul
        style={{
          marginTop: '12px',
          paddingLeft: '20px',
          fontSize: '0.8rem',
          color: 'var(--muted-2)',
          fontFamily: 'var(--sans)',
        }}
      >
        {artifacts.map((a, i) => (
          <li key={i}>{a.artifact_role}: {a.relpath}</li>
        ))}
      </ul>
    </div>
  );
}
