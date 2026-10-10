import { describe, expect, test } from 'bun:test'
import { capturePersistenceKey, seedCaptureItems, captureDraftItems, applyGeneratedTitle, applyTitleGenerationError, parseCaptureLink, resolveCaptureLink, makeCaptureLinkRequest } from './captureRequests'

describe('capture request drafts', () => {
  test('separate requests never persist over the ordinary capture draft', () => {
    expect(capturePersistenceKey('captureItems')).toBe('captureItems')
    expect(capturePersistenceKey('captureItems', [{ locator: 'https://example.com' }], 'one')).not.toBe('captureItems')
    expect(capturePersistenceKey('captureItems', [], 'two')).not.toBe(capturePersistenceKey('captureItems', [], 'one'))
    expect(capturePersistenceKey('extension-draft', [], 'one')).toBe('extension-draft')
  })
  test('seeding preserves body bytes and only allows supported fields', () => {
    const seeds = seedCaptureItems([{ kind: 'text', title: 'Draft', body: ' \n body\n', mime: 'text/plain', status: 'completed' }, { locator: 'https://example.com', quality: 'bad' }])
    expect(seeds).toEqual([{ kind: 'text', title: 'Draft', body: ' \n body\n', mime: 'text/plain' }, { locator: 'https://example.com' }])
  })
  test('a delayed title never overwrites edits, including edits changed back', () => {
    const snapshot = { title: 'Draft', body: 'Body', draftRevision: 2, titleRequestId: 7 }
    expect(applyGeneratedTitle({ ...snapshot, titleBusy: true }, snapshot, 'Generated').title).toBe('Generated')
    const edited = { ...snapshot, title: 'My title', body: 'New body', draftRevision: 3, titleBusy: true }
    expect(applyGeneratedTitle(edited, snapshot, 'Generated')).toMatchObject({ title: 'My title', body: 'New body', titleBusy: false })
    expect(applyGeneratedTitle({ ...snapshot, draftRevision: 4 }, snapshot, 'Generated').title).toBe('Draft')
    expect(applyGeneratedTitle({ ...snapshot, titleRequestId: 8, titleBusy: true }, snapshot, 'Generated').titleBusy).toBe(true)
  })
  test('remount restores a matching seeded request, while a new request starts from its seed', () => {
    const saved = { requestKey: 'one', items: [{ kind: 'text', title: 'Edited', body: 'Kept' }] }
    const seed = [{ kind: 'text', title: 'Seed', body: 'Original' }]
    expect(captureDraftItems(saved, seed, 'one')).toEqual(saved.items)
    expect(captureDraftItems(saved, seed, 'two')).toEqual(seedCaptureItems(seed))
    expect(captureDraftItems([{ locator: 'ordinary-draft' }], seed, 'one')).toEqual(seedCaptureItems(seed))
    expect(captureDraftItems([{ locator: 'ordinary-draft' }])).toEqual([{ locator: 'ordinary-draft' }])
  })
  test('title failures keep every current draft edit and ignore an older request', () => {
    const current = { title: 'Edited', body: '  Body\n', mime: 'text/plain', titleBusy: true, titleRequestId: 9 }
    expect(applyTitleGenerationError(current, 9, 'Failed')).toEqual({ ...current, titleBusy: false, titleError: 'Failed' })
    expect(applyTitleGenerationError(current, 8, 'Old failure')).toBe(current)
  })
})

describe('capture deep links', () => {
  test('decodes a locator exactly once and keeps the pinned archive', () => {
    const link = parseCaptureLink('?archive=personal&capture=https%3A%2F%2Fexample.com%2F%3Fq%3D%252F')
    expect(resolveCaptureLink(link, [{ id: 'other' }, { id: 'personal' }])).toEqual({ archiveId: 'personal', locator: 'https://example.com/?q=%2F' })
    expect(parseCaptureLink('?archive=personal')).toBeNull()
  })
  test('missing and invalid pinned archives cannot fall back to another archive', () => {
    expect(resolveCaptureLink(parseCaptureLink('?capture=https%3A%2F%2Fexample.com'), [{ id: 'other' }]).error).toContain('archive')
    expect(resolveCaptureLink(parseCaptureLink('?archive=missing&capture=x'), [{ id: 'other' }]).error).toContain('missing')
    expect(resolveCaptureLink(parseCaptureLink('?archive=other&capture='), [{ id: 'other' }]).error).toContain('locator')
  })
  test('each navigation uses a fresh request even when a prior link has persisted edits', () => {
    const old = makeCaptureLinkRequest({ archiveId: 'personal', locator: 'https://old.example' })
    const saved = { requestKey: old.requestKey, items: [{ locator: 'https://edited-old.example' }] }
    const next = makeCaptureLinkRequest(resolveCaptureLink(parseCaptureLink('?archive=personal&capture=https%3A%2F%2Fnew.example'), [{ id: 'personal' }]))
    expect(next.requestKey).not.toBe(old.requestKey)
    expect(captureDraftItems(saved, next.initialItems, next.requestKey)).toEqual([{ locator: 'https://new.example' }])
    expect(makeCaptureLinkRequest({ archiveId: 'personal', locator: 'https://new.example' }).requestKey).not.toBe(next.requestKey)
  })
})
