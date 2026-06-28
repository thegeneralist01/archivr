import Topbar from './Topbar';

export default {
  component: Topbar,
  tags: ['autodocs'],
};

const defaultArchives = [
  { id: '1', label: 'Main Archive' },
  { id: '2', label: 'Research' },
  { id: '3', label: 'Screenshots' },
];

export const Default = {
  args: {
    archives: defaultArchives,
    archiveId: '1',
    onArchiveChange: () => {},
    view: 'archive',
    onViewChange: () => {},
    onCaptureClick: () => {},
  },
};

export const WithUser = {
  args: {
    ...Default.args,
  },
  decorators: [
    (Story) => (
      <div style={{ background: 'var(--paper)' }}>
        <Story />
      </div>
    ),
  ],
};
