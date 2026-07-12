import { useEffect, useRef } from 'react';

export default function VideoPreview({ src }) {
  const videoRef = useRef(null);

  useEffect(() => {
    if (videoRef.current) {
      videoRef.current.load();
    }
  }, [src]);

  return (
    <div
      className="preview-video-wrap"
      style={{
        background: '#111',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        width: '100%',
        height: '100%',
        minHeight: '240px',
      }}
    >
      {src ? (
        <video
          ref={videoRef}
          controls
          autoPlay={false}
          style={{ width: '100%', maxHeight: '100%', display: 'block' }}
        >
          <source src={src} />
          Your browser does not support the video element.
        </video>
      ) : (
        <span style={{ color: 'var(--muted)', fontFamily: 'var(--sans)', fontSize: '0.9rem' }}>
          No video available
        </span>
      )}
    </div>
  );
}
