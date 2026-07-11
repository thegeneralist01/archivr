import { useEffect, useRef, useState } from 'react';
import { sourceIconSvg } from '../utils';

function formatTime(secs) {
  if (!isFinite(secs) || isNaN(secs)) return '--:--';
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return `${m}:${String(s).padStart(2, '0')}`;
}

export default function AudioBar({ entry, src, archiveId, onClose }) {
  const audioRef = useRef(null);
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(NaN);
  const [volume, setVolume] = useState(1.0);

  // Load and reset state whenever src changes — play only on explicit user action
  useEffect(() => {
    if (!audioRef.current || !src) return;
    audioRef.current.load();
    setCurrentTime(0);
    setDuration(NaN);
    setIsPlaying(false);
  }, [src]);

  // Sync volume to audio element
  useEffect(() => {
    if (audioRef.current) {
      audioRef.current.volume = volume;
    }
  }, [volume]);

  function handlePlayPause() {
    const audio = audioRef.current;
    if (!audio) return;
    if (isPlaying) {
      audio.pause();
    } else {
      audio.play().catch(() => {});
    }
  }

  function handleSeek(e) {
    const audio = audioRef.current;
    if (!audio || !isFinite(duration)) return;
    const t = Number(e.target.value);
    audio.currentTime = t;
    setCurrentTime(t);
  }

  function handleVolumeChange(e) {
    setVolume(Number(e.target.value));
  }

  const title = entry?.title || entry?.entry_uid || 'Unknown';
  const kind = entry?.source_kind || 'other';

  return (
    <>
      {/* Hidden audio element */}
      <audio
        ref={audioRef}
        src={src || undefined}
        preload="auto"
        onPlay={() => setIsPlaying(true)}
        onPause={() => setIsPlaying(false)}
        onEnded={() => setIsPlaying(false)}
        onTimeUpdate={() => setCurrentTime(audioRef.current?.currentTime ?? 0)}
        onLoadedMetadata={() => setDuration(audioRef.current?.duration ?? NaN)}
        onDurationChange={() => setDuration(audioRef.current?.duration ?? NaN)}
        style={{ display: 'none' }}
      />

      {/* Fixed bottom bar */}
      <div
        className="audio-bar"
        style={{
          position: 'fixed',
          bottom: 0,
          left: 0,
          right: 0,
          zIndex: 100,
          background: 'var(--paper-3)',
          borderTop: '1px solid var(--line)',
          display: 'flex',
          alignItems: 'center',
          gap: '16px',
          padding: '0 16px',
          height: '56px',
          fontFamily: 'var(--sans)',
        }}
      >
        {/* Left: icon + title */}
        <div
          className="audio-bar-info"
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: '8px',
            minWidth: 0,
            flex: '0 1 220px',
            overflow: 'hidden',
          }}
        >
          <span
            className="source-icon"
            style={{ flexShrink: 0, width: '18px', height: '18px', display: 'flex', alignItems: 'center' }}
            dangerouslySetInnerHTML={{ __html: sourceIconSvg(kind) }}
          />
          <span
            style={{
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
              fontSize: '0.85rem',
              color: 'var(--ink)',
            }}
            title={title}
          >
            {title}
          </span>
        </div>

        {/* Center: play/pause + seek + time */}
        <div
          className="audio-bar-controls"
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: '10px',
            flex: '1 1 0',
            minWidth: 0,
          }}
        >
          <button
            onClick={handlePlayPause}
            aria-label={isPlaying ? 'Pause' : 'Play'}
            style={{
              background: 'none',
              border: 'none',
              cursor: 'pointer',
              padding: '4px',
              color: 'var(--ink)',
              flexShrink: 0,
              fontSize: '1.2rem',
              lineHeight: 1,
            }}
          >
            {isPlaying ? '⏸' : '▶'}
          </button>

          <span
            style={{ fontSize: '0.75rem', color: 'var(--muted)', whiteSpace: 'nowrap', flexShrink: 0 }}
          >
            {formatTime(currentTime)}
          </span>

          <input
            type="range"
            min={0}
            max={isFinite(duration) ? duration : 0}
            step={0.1}
            value={isFinite(currentTime) ? currentTime : 0}
            onChange={handleSeek}
            aria-label="Seek"
            style={{ flex: 1, minWidth: 0, accentColor: 'var(--accent)' }}
          />

          <span
            style={{ fontSize: '0.75rem', color: 'var(--muted)', whiteSpace: 'nowrap', flexShrink: 0 }}
          >
            {formatTime(duration)}
          </span>
        </div>

        {/* Right: volume + close */}
        <div
          className="audio-bar-right"
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: '8px',
            flex: '0 1 160px',
          }}
        >
          <span style={{ fontSize: '0.85rem', color: 'var(--muted)', flexShrink: 0 }}>🔊</span>
          <input
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={volume}
            onChange={handleVolumeChange}
            aria-label="Volume"
            style={{ width: '80px', accentColor: 'var(--accent)' }}
          />
          <button
            onClick={onClose}
            aria-label="Close audio player"
            style={{
              background: 'none',
              border: 'none',
              cursor: 'pointer',
              padding: '4px',
              color: 'var(--muted)',
              fontSize: '1rem',
              lineHeight: 1,
              marginLeft: '4px',
            }}
          >
            ✕
          </button>
        </div>
      </div>
    </>
  );
}
