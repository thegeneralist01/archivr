import EntryRow from './EntryRow';

export default {
  component: EntryRow,
  tags: ['autodocs'],
};

const sampleEntry = {
  entry_uid: 'entry_123',
  title: 'A Great Article About Web Design',
  archived_at: '2026-06-27T10:30:00Z',
  entity_kind: 'page',
  source_kind: 'web',
  total_artifact_bytes: 2048576,
  original_url: 'https://example.com/article',
  has_favicon: false,
};

const videoEntry = {
  entry_uid: 'entry_456',
  title: 'React Performance Tips',
  archived_at: '2026-06-26T14:15:00Z',
  entity_kind: 'video',
  source_kind: 'youtube',
  total_artifact_bytes: 104857600,
  original_url: 'https://youtube.com/watch?v=xyz',
  has_favicon: false,
};

export const Default = {
  args: {
    entry: sampleEntry,
    archiveId: 'archive_1',
    isSelected: false,
    onSelect: () => {},
  },
  decorators: [
    (Story) => (
      <div style={{
        display: 'grid',
        gridTemplateColumns: '178px 38% 130px 110px 34%',
        gap: '10px',
        padding: '10px',
        background: 'var(--paper-3)',
      }}>
        <Story />
      </div>
    ),
  ],
};

export const Selected = {
  args: {
    ...Default.args,
    isSelected: true,
  },
  decorators: Default.decorators,
};

export const Video = {
  args: {
    ...Default.args,
    entry: videoEntry,
  },
  decorators: Default.decorators,
};
