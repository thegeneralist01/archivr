import { useEffect, useRef, useState } from 'react';
import { issueMediaToken } from '../api';

// Regex to extract (archiveId, entryUid, artifactIndex) from an artifact path.
const ARTIFACT_URL_RE =
  /\/api\/archives\/([^/]+)\/entries\/([^/]+)\/artifacts\/(\d+)/;

// ── AirPlay icon (screen with upward triangle) ─────────────────────────────
function AirPlayIcon() {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M5 17H3a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h18a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2h-2" />
      <polygon points="12 15 17 21 7 21 12 15" />
    </svg>
  );
}

// ─────────────────────────────────────────────────────────────────────────────

export default function VideoPreview({ src, contentType = 'video/mp4' }) {
  const videoRef        = useRef(null);
  // Signed URL that Cast devices and Apple TV can fetch without a session cookie.
  const [signedSrc,    setSignedSrc]    = useState(null);
  const [castReady,    setCastReady]    = useState(false);
  const [airplayReady, setAirplayReady] = useState(false);

  // ── Pre-fetch signed media token whenever src changes ───────────────────
  // The video element uses the signed URL so AirPlay (Apple TV fetches it
  // directly) and Cast (we pass it to loadMedia) both work without auth.
  useEffect(() => {
    if (!src) { setSignedSrc(null); return; }
    const m = src.match(ARTIFACT_URL_RE);
    if (!m) { setSignedSrc(src); return; } // not a recognised artifact URL
    let cancelled = false;
    setSignedSrc(null); // reset while fetching
    issueMediaToken(m[1], m[2], parseInt(m[3], 10))
      .then(({ url }) => { if (!cancelled) setSignedSrc(url); })
      .catch(() => { if (!cancelled) setSignedSrc(src); }); // fallback on error
    return () => { cancelled = true; };
  }, [src]);

  // Reload video element when the active source changes.
  useEffect(() => {
    if (videoRef.current) videoRef.current.load();
  }, [signedSrc]);

  // ── Detect AirPlay (Safari / WebKit only) ───────────────────────────────
  useEffect(() => {
    setAirplayReady(
      typeof HTMLVideoElement !== 'undefined' &&
      'webkitShowPlaybackTargetPicker' in HTMLVideoElement.prototype
    );
  }, []);

  // ── Bootstrap Cast SDK (lazy-injected, only when a video is displayed) ──
  // The SDK calls window.__onGCastApiAvailable once it finishes loading.
  // CSP: script-src includes https://www.gstatic.com (see security_headers).
  useEffect(() => {
    const init = (isAvailable) => {
      if (!isAvailable) return;
      try {
        /* global cast, chrome */
        cast.framework.CastContext.getInstance().setOptions({
          receiverApplicationId: chrome.cast.media.DEFAULT_MEDIA_RECEIVER_APP_ID,
          autoJoinPolicy: chrome.cast.AutoJoinPolicy.ORIGIN_SCOPED,
        });
        setCastReady(true);
      } catch (_) {
        // SDK unavailable (HTTP page, no Cast extension, etc.) — hide button silently.
      }
    };

    if (window.cast?.framework) {
      init(true); // SDK already loaded from a previous video view.
    } else {
      window.__onGCastApiAvailable = init;
      if (!document.querySelector('script[src*="cast_sender"]')) {
        const s  = document.createElement('script');
        s.src    = 'https://www.gstatic.com/cv/js/sender/v1/cast_sender.js?loadCastFramework=1';
        document.head.appendChild(s);
      }
    }
  }, []);

  // ── Send video to Cast session when connected / resumed ─────────────────
  useEffect(() => {
    if (!castReady || !signedSrc) return;

    const ctx = cast.framework.CastContext.getInstance();
    const { CastContextEventType, SessionState } = cast.framework;

    const onSessionState = (event) => {
      if (
        event.sessionState !== SessionState.SESSION_STARTED &&
        event.sessionState !== SessionState.SESSION_RESUMED
      ) return;
      const session = ctx.getCurrentSession();
      if (!session) return;
      const mediaInfo = new chrome.cast.media.MediaInfo(
        window.location.origin + signedSrc,
        contentType
      );
      session
        .loadMedia(new chrome.cast.media.LoadRequest(mediaInfo))
        .catch(() => {});
    };

    ctx.addEventListener(CastContextEventType.SESSION_STATE_CHANGED, onSessionState);
    return () => ctx.removeEventListener(CastContextEventType.SESSION_STATE_CHANGED, onSessionState);
  }, [castReady, signedSrc, contentType]);

  // ── Handlers ────────────────────────────────────────────────────────────
  const handleAirPlay = () => {
    videoRef.current?.webkitShowPlaybackTargetPicker?.();
  };

  const showOverlay = castReady || airplayReady;

  return (
    <div className="preview-video-wrap" style={{ position: 'relative' }}>
      {src ? (
        <>
          {signedSrc ? (
            <video
              ref={videoRef}
              controls
              autoPlay={false}
              playsInline
              x-webkit-airplay="allow"
              style={{ width: '100%', maxHeight: '100%', display: 'block' }}
            >
              <source src={signedSrc} />
              Your browser does not support the video element.
            </video>
          ) : (
            // Brief loading state while signed URL is being fetched.
            <div className="video-tv-loading" aria-label="Loading video…" />
          )}

          {showOverlay && signedSrc && (
            <div className="video-tv-controls">
              {airplayReady && (
                <button
                  className="video-tv-btn"
                  title="AirPlay to device"
                  aria-label="AirPlay to device"
                  onClick={handleAirPlay}
                >
                  <AirPlayIcon />
                </button>
              )}
              {castReady && (
                // google-cast-launcher is a web component from the Cast SDK.
                // It manages its own connection-state icon automatically.
                // eslint-disable-next-line react/no-unknown-property
                <google-cast-launcher
                  className="video-tv-btn"
                  title="Cast to TV"
                />
              )}
            </div>
          )}
        </>
      ) : (
        <span style={{ color: 'var(--muted)', fontFamily: 'var(--sans)', fontSize: '0.9rem' }}>
          No video available
        </span>
      )}
    </div>
  );
}
