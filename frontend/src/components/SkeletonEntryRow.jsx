const COLLECTION_MARKERS = [
  'list=',
  '/playlist/',
  '/channel/',
  'yt:playlist:',
  'yt:channel:',
  'ytm:playlist:',
  'spotify:playlist:',
  'spotify:album:',
];

function isCollectionLocator(locator) {
  const normalized = locator.toLowerCase();
  return COLLECTION_MARKERS.some(marker => normalized.includes(marker));
}

function truncateLocator(locator) {
  return locator.length > 80 ? `${locator.slice(0, 79)}…` : locator;
}

export default function SkeletonEntryRow({ locator = '' }) {
  const locatorText = String(locator);
  const isCollection = isCollectionLocator(locatorText);

  return (
    <div className="in-progress-entry-row" role="status" aria-live="polite">
      <span className="cap-spinner in-progress-entry-row__spinner" aria-hidden="true" />
      <span className="in-progress-entry-row__locator" title={locatorText}>
        {truncateLocator(locatorText)}
      </span>
      <span className="in-progress-entry-row__status">
        Archiving…
        {isCollection && <span className="in-progress-entry-row__kind">(playlist)</span>}
      </span>
    </div>
  );
}
