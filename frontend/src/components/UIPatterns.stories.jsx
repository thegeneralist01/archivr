export default {
  title: 'UI Patterns',
  tags: ['autodocs'],
};

export const Buttons = () => (
  <div style={{ padding: '20px', display: 'flex', gap: '16px', flexWrap: 'wrap' }}>
    <button className="capture-button">+ Capture</button>
    <button className="nav-link">Nav Link</button>
    <button className="nav-link is-active">Nav Link (Active)</button>
    <button className="assign-tag-btn">Add Tag</button>
    <button className="assign-tag-btn" style={{ pointerEvents: 'none', opacity: 0.6 }}>Disabled</button>
  </div>
);

export const FormInputs = () => (
  <div style={{ padding: '20px', maxWidth: '400px', display: 'flex', flexDirection: 'column', gap: '16px' }}>
    <div>
      <label style={{ display: 'block', marginBottom: '6px', color: 'var(--muted)', fontSize: '12px' }}>Search Input</label>
      <input className="search-input" type="search" placeholder="Search archive..." />
    </div>
    <div>
      <label style={{ display: 'block', marginBottom: '6px', color: 'var(--muted)', fontSize: '12px' }}>Capture Input</label>
      <input className="capture-input" type="text" placeholder="tweet:1234567890 or https://..." />
    </div>
    <div>
      <label style={{ display: 'block', marginBottom: '6px', color: 'var(--muted)', fontSize: '12px' }}>Tag Input</label>
      <input className="assign-tag-input" type="text" placeholder="/science/cs" />
    </div>
    <div>
      <label style={{ display: 'block', marginBottom: '6px', color: 'var(--muted)', fontSize: '12px' }}>Archive Switcher</label>
      <select className="archive-switcher" style={{ width: '100%' }}>
        <option>Main Archive</option>
        <option>Research</option>
        <option>Screenshots</option>
      </select>
    </div>
  </div>
);

export const Pills = () => (
  <div style={{ padding: '20px', display: 'flex', gap: '8px', flexWrap: 'wrap' }}>
    <span className="type-pill">page</span>
    <span className="type-pill">video</span>
    <span className="type-pill">tweet_thread</span>
    <span className="type-pill">file</span>
  </div>
);

export const TagPills = () => (
  <div style={{ padding: '20px', display: 'flex', gap: '8px', flexWrap: 'wrap' }}>
    <span className="tag-pill">
      science
      <button className="remove-tag">×</button>
    </span>
    <span className="tag-pill">
      computer-science
      <button className="remove-tag">×</button>
    </span>
    <span className="tag-pill">
      learning
      <button className="remove-tag">×</button>
    </span>
  </div>
);

export const ColorPalette = () => (
  <div style={{ padding: '20px', display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(200px, 1fr))', gap: '16px' }}>
    <div>
      <div style={{ height: '80px', background: 'var(--ink)', marginBottom: '8px', borderRadius: '4px' }} />
      <strong>Ink</strong> (#20251f)
    </div>
    <div>
      <div style={{ height: '80px', background: 'var(--paper)', border: '1px solid var(--line)', marginBottom: '8px', borderRadius: '4px' }} />
      <strong>Paper</strong> (#f5f0e7)
    </div>
    <div>
      <div style={{ height: '80px', background: 'var(--accent)', marginBottom: '8px', borderRadius: '4px' }} />
      <strong>Accent</strong> (#8d3f30)
    </div>
    <div>
      <div style={{ height: '80px', background: 'var(--accent-2)', marginBottom: '8px', borderRadius: '4px' }} />
      <strong>Accent 2</strong> (#b78342)
    </div>
    <div>
      <div style={{ height: '80px', background: 'var(--link)', marginBottom: '8px', borderRadius: '4px' }} />
      <strong>Link</strong> (#245f72)
    </div>
    <div>
      <div style={{ height: '80px', background: 'var(--top)', marginBottom: '8px', borderRadius: '4px' }} />
      <strong>Top</strong> (#141d18)
    </div>
    <div>
      <div style={{ height: '80px', background: 'var(--muted)', marginBottom: '8px', borderRadius: '4px' }} />
      <strong>Muted</strong> (#666a61)
    </div>
    <div>
      <div style={{ height: '80px', background: 'var(--line)', marginBottom: '8px', borderRadius: '4px' }} />
      <strong>Line</strong> (#d2c6b5)
    </div>
  </div>
);
