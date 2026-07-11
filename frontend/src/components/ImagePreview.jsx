export default function ImagePreview({ src, alt }) {
  return (
    <div
      className="preview-image-wrap"
      style={{
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        width: '100%',
        height: '100%',
        background: 'var(--paper-2)',
        overflow: 'hidden',
      }}
    >
      <a href={src} target="_blank" rel="noreferrer noopener" style={{ display: 'contents' }}>
        <img
          src={src}
          alt={alt || ''}
          style={{
            objectFit: 'contain',
            maxHeight: '100%',
            maxWidth: '100%',
            display: 'block',
            cursor: 'pointer',
          }}
        />
      </a>
    </div>
  );
}
