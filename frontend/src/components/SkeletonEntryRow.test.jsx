import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import SkeletonEntryRow from './SkeletonEntryRow';

describe('SkeletonEntryRow', () => {
  test('renders a compact archiving status row for a locator', () => {
    const markup = renderToStaticMarkup(
      <SkeletonEntryRow locator="tweet:1891234567890123456" />,
    );

    expect(markup).toContain('in-progress-entry-row__spinner');
    expect(markup).toContain('tweet:1891234567890123456');
    expect(markup).toContain('Archiving…');
    expect(markup).not.toContain('(playlist)');
  });

  test('labels playlist and channel locators', () => {
    const collectionLocators = [
      'https://www.youtube.com/watch?v=abc123&list=PL123',
      'https://www.youtube.com/playlist/PL123',
      'https://www.youtube.com/channel/UC123',
      'yt:playlist:PL123',
      'yt:channel:UC123',
      'ytm:playlist:PL123',
      'spotify:playlist:123',
      'spotify:album:123',
    ];

    for (const locator of collectionLocators) {
      const markup = renderToStaticMarkup(<SkeletonEntryRow locator={locator} />);
      expect(markup).toContain('(playlist)');
    }
  });

  test('truncates long locators to 80 displayed characters', () => {
    const locator = 'x'.repeat(100);
    const markup = renderToStaticMarkup(<SkeletonEntryRow locator={locator} />);
    const visibleLocator = markup.match(/in-progress-entry-row__locator"[^>]*>(.*?)<\/span>/)?.[1];

    expect(visibleLocator).toBe(`${'x'.repeat(79)}…`);
  });
});
