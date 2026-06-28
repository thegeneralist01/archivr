import { useState } from 'react';
import CaptureDialog from './CaptureDialog';

export default {
  component: CaptureDialog,
  tags: ['autodocs'],
};

function CaptureDialogWrapper(args) {
  const [open, setOpen] = useState(args.open);

  return (
    <div>
      <button onClick={() => setOpen(true)} style={{ padding: '8px 16px', marginBottom: '16px' }}>
        Open Capture Dialog
      </button>
      <CaptureDialog
        {...args}
        open={open}
        onClose={() => setOpen(false)}
      />
    </div>
  );
}

export const Default = {
  render: (args) => <CaptureDialogWrapper {...args} />,
  args: {
    open: false,
    archiveId: 'archive_1',
    onCaptured: () => {},
  },
};

export const Open = {
  args: {
    open: true,
    archiveId: 'archive_1',
    onCaptured: () => {},
  },
};
