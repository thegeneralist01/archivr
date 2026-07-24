import SkeletonEntryRow from './SkeletonEntryRow';

import EntryRow from './EntryRow';

export default function EntriesView({ entries, selectedUids, onRowClick, archiveId, pendingCaptures = [], deletedUids, isPublicSession }) {
  return (
    <section id="archive-view" className="view is-active">
      <div className="entry-table">
        <div className="entry-header-row">
          <div className="col-check" aria-hidden="true" />
          <div className="col-added">Added</div>
          <div className="col-title">Title</div>
          <div className="col-type">Type</div>
          <div className="col-size">Size</div>
          <div className="col-url">Original URL</div>
        </div>
        <div id="entries-body">
          {pendingCaptures.filter(c => c.archiveId === archiveId).reverse().map(cap => (
            <SkeletonEntryRow key={cap.id} />
          ))}
          {entries.map((entry, idx) => (
            <EntryRow
              key={entry.entry_uid}
              entry={entry}
              rowIndex={idx}
              archiveId={archiveId}
              isSelected={selectedUids.size === 1 && selectedUids.has(entry.entry_uid)}
              isMultiSelected={selectedUids.size >= 2 && selectedUids.has(entry.entry_uid)}
              onRowClick={onRowClick}
              selectedUids={selectedUids}
              deletedUids={deletedUids}
              isPublicSession={isPublicSession}
            />
          ))}
        </div>
      </div>
    </section>
  );
}
