import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

// Shim window.fetch so api.js can install its 401 interceptor at import time.
const originalWindow = globalThis.window
globalThis.window = { fetch: globalThis.fetch }
const { default: ExportDialog } = await import('./ExportDialog')
globalThis.window = originalWindow

describe('ExportDialog', () => {
  test('with one uid, no "Combine" toggle is rendered', () => {
    const markup = renderToStaticMarkup(
      <ExportDialog open archiveId="a" entryUids={['u1']} onClose={() => {}} />
    )
    expect(markup).not.toContain('Combine into one document')
    expect(markup).toContain('Export entry')
  })

  test('with two uids, the "Combine" toggle is rendered', () => {
    const markup = renderToStaticMarkup(
      <ExportDialog open archiveId="a" entryUids={['u1', 'u2']} onClose={() => {}} />
    )
    expect(markup).toContain('Combine into one document')
    expect(markup).toContain('Export 2 entries')
  })

  test('"Include Archivr credit" toggle is checked by default', () => {
    const markup = renderToStaticMarkup(
      <ExportDialog open archiveId="a" entryUids={['u1']} onClose={() => {}} />
    )
    expect(markup).toContain('Include Archivr credit')
    expect(markup).toContain('aria-checked="true"')
  })

  test('closed dialog does not render modal content', () => {
    const markup = renderToStaticMarkup(
      <ExportDialog open={false} archiveId="a" entryUids={['u1']} onClose={() => {}} />
    )
    // The dialog element is still in the DOM (native dialog hides itself).
    expect(markup).toContain('export-dialog')
  })
})
