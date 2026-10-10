import { afterEach, describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

// api.js installs its browser auth interceptor at module load. Rendering here
// exercises only React's initial state, so no live browser requests are needed.
const originalWindow = globalThis.window
globalThis.window = { fetch: globalThis.fetch }
const { default: CaptureDialog, CaptureTextRow } = await import('./CaptureDialog')
globalThis.window = originalWindow

const originalStorage = globalThis.sessionStorage
afterEach(() => { globalThis.sessionStorage = originalStorage })

function storage(values = {}) {
  return {
    getItem: key => values[key] ?? null,
    setItem: (key, value) => { values[key] = value },
    removeItem: key => { delete values[key] },
  }
}

describe('shared CaptureDialog', () => {
  test('renders a separate text seed without submitting or reading the ordinary draft', () => {
    globalThis.sessionStorage = storage({ captureItems: JSON.stringify([{ locator: 'ordinary-draft' }]) })
    let calls = 0
    const api = new Proxy({}, { get: () => () => { calls++; throw new Error('Unexpected automatic API call') } })
    const markup = renderToStaticMarkup(<CaptureDialog open archiveId="personal" api={api} requestKey="request-one" initialItems={[{ kind: 'text', title: 'Selected text', body: '  Body\n', mime: 'text/plain' }]} allowFileUpload={false} />)
    expect(markup).toContain('Selected text')
    expect(markup).toContain('  Body\n')
    expect(markup).not.toContain('ordinary-draft')
    expect(markup).not.toContain('Upload file')
    expect(markup).not.toContain('type="file"')
    expect(calls).toBe(0)
    expect(globalThis.sessionStorage.getItem('captureItems')).toContain('ordinary-draft')
  })
  test('keeps ordinary drafts and file uploads available by default', () => {
    globalThis.sessionStorage = storage({ captureItems: JSON.stringify([{ locator: 'ordinary-draft', status: 'idle' }]) })
    const markup = renderToStaticMarkup(<CaptureDialog open archiveId="personal" />)
    expect(markup).toContain('ordinary-draft')
    expect(markup).toContain('Upload file')
  })
  test('restores edited rows from a matching seeded request on remount', () => {
    globalThis.sessionStorage = storage({ 'captureItems:request:one': JSON.stringify({ requestKey: 'one', items: [{ kind: 'text', title: 'Edited title', body: 'Edited body', mime: 'text/plain' }] }) })
    const markup = renderToStaticMarkup(<CaptureDialog open archiveId="personal" requestKey="one" initialItems={[{ kind: 'text', title: 'Original title', body: 'Original body' }]} />)
    expect(markup).toContain('Edited title')
    expect(markup).toContain('Edited body')
    expect(markup).not.toContain('Original title')
  })

  test('does not render stale probe results for a full X status URL', () => {
    const locator = 'https://x.com/cognition/status/2107165034463867001/photo/1?ref=share#media'
    const requestKey = 'tweet-empty-probe'
    globalThis.sessionStorage = storage({
      [`captureItems:request:${requestKey}`]: JSON.stringify({
        requestKey,
        items: [{ locator, probeState: 'done', probeQualities: [], probeHasAudio: false }],
      }),
    })
    const markup = renderToStaticMarkup(
      <CaptureDialog
        open
        archiveId="personal"
        requestKey={requestKey}
        initialItems={[{ locator }]}
      />
    )

    expect(markup).toContain(locator)
    expect(markup).not.toContain('No media detected')
    expect(markup).not.toContain('aria-label="Video quality"')
  })

  test('does not render stale available qualities for an X status URL', () => {
    const locator = 'https://x.com/cognition/status/2107165034463867001/video/1'
    const requestKey = 'tweet-nonempty-probe'
    globalThis.sessionStorage = storage({
      [`captureItems:request:${requestKey}`]: JSON.stringify({
        requestKey,
        items: [{ locator, probeState: 'done', probeQualities: ['1080p'], probeHasAudio: true, quality: '1080p' }],
      }),
    })
    const markup = renderToStaticMarkup(
      <CaptureDialog
        open
        archiveId="personal"
        requestKey={requestKey}
        initialItems={[{ locator }]}
      />
    )

    expect(markup).toContain(locator)
    expect(markup).not.toContain('1080p')
    expect(markup).not.toContain('aria-label="Video quality"')
  })

  test('keeps a full tweet archive action enabled despite a restored probing state', () => {
    const locator = 'https://www.twitter.com/cognition/status/2107165034463867001'
    const requestKey = 'tweet-probing'
    globalThis.sessionStorage = storage({
      [`captureItems:request:${requestKey}`]: JSON.stringify({
        requestKey,
        items: [{ locator, probeState: 'probing' }],
      }),
    })
    const markup = renderToStaticMarkup(
      <CaptureDialog open archiveId="personal" requestKey={requestKey} initialItems={[{ locator }]} />
    )

    expect(markup).not.toContain('Checking available qualities')
    expect(markup).toContain('<button type="button" class="capture-submit">Archive</button>')
  })

  test('hides stale probe UI for X and Twitter status URL host variants', () => {
    const requestKey = 'tweet-host-variants'
    const locators = [
      'https://www.x.com/cognition/status/2107165034463867001?ref=share',
      'https://mobile.x.com/cognition/status/2107165034463867001/photo/1#media',
      'https://m.x.com/cognition/status/2107165034463867001',
      'https://twitter.com/cognition/status/2107165034463867001',
      'https://www.twitter.com/cognition/status/2107165034463867001/video/1',
      'https://mobile.twitter.com/cognition/status/2107165034463867001',
      'https://m.twitter.com/cognition/status/2107165034463867001',
    ]
    globalThis.sessionStorage = storage({
      [`captureItems:request:${requestKey}`]: JSON.stringify({
        requestKey,
        items: locators.map(locator => ({ locator, probeState: 'done', probeQualities: [], probeHasAudio: false })),
      }),
    })
    const markup = renderToStaticMarkup(
      <CaptureDialog open archiveId="personal" requestKey={requestKey} initialItems={locators.map(locator => ({ locator }))} />
    )

    for (const locator of locators) expect(markup).toContain(locator)
    expect(markup).not.toContain('No media detected')
    expect(markup).not.toContain('No media available')
    expect(markup).not.toContain('aria-label="Video quality"')
  })

  test('treats tweet and thread shorthands as whole-tweet captures', () => {
    const requestKey = 'tweet-shorthands'
    const locators = ['tweet:2107165034463867001', 'x:tweet:2107165034463867001', 'twitter:thread:2107165034463867001', 'tweet:thread:2107165034463867001']
    globalThis.sessionStorage = storage({
      [`captureItems:request:${requestKey}`]: JSON.stringify({
        requestKey,
        items: locators.map(locator => ({ locator, probeState: 'done', probeQualities: [], probeHasAudio: false })),
      }),
    })
    const markup = renderToStaticMarkup(
      <CaptureDialog open archiveId="personal" requestKey={requestKey} initialItems={locators.map(locator => ({ locator }))} />
    )

    expect(markup).not.toContain('No media detected')
    expect(markup).not.toContain('aria-label="Video quality"')
  })

  test('keeps explicit X, tweet, and Twitter media shorthands quality-selectable', () => {
    const requestKey = 'x-media-probe'
    const locators = ['x:media:2107165034463867001', 'tweet:media:2107165034463867001', 'twitter:media:2107165034463867001']
    globalThis.sessionStorage = storage({
      [`captureItems:request:${requestKey}`]: JSON.stringify({
        requestKey,
        items: locators.map(locator => ({ locator, probeState: 'done', probeQualities: ['1080p'], probeHasAudio: true, quality: '1080p' })),
      }),
    })
    const markup = renderToStaticMarkup(
      <CaptureDialog
        open
        archiveId="personal"
        requestKey={requestKey}
        initialItems={locators.map(locator => ({ locator }))}
      />
    )

    expect(markup.match(/aria-label="Video quality"/g)).toHaveLength(locators.length)
    expect(markup).toContain('1080p')
  })
})

describe('manual title controls', () => {
  const providers = [{ kind: 'codex_cli', label: 'Codex' }, { kind: 'anthropic', label: 'Anthropic' }]
  test('shows available providers and generation separately from editable title/body', () => {
    const markup = renderToStaticMarkup(<CaptureTextRow item={{ title: 'My title', body: 'My body', mime: 'text/plain' }} titleProviders={providers} titleProvider="anthropic" />)
    expect(markup).toContain('Generate title')
    expect(markup).toContain('Title provider')
    expect(markup).toContain('value="anthropic" selected=""')
    expect(markup).toContain('value="My title"')
    expect(markup).toContain('My body')
  })
  test('disables repeated generation while showing failures beside preserved drafts', () => {
    const markup = renderToStaticMarkup(<CaptureTextRow item={{ title: 'Edited', body: 'New body', mime: 'text/plain', titleBusy: true, titleError: 'Provider failed' }} titleProviders={providers} titleProvider="codex_cli" />)
    expect(markup).toContain('disabled="">Generating…')
    expect(markup).toContain('role="alert">Provider failed')
    expect(markup).toContain('value="Edited"')
    expect(markup).toContain('New body')
  })
})
