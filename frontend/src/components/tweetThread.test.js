import { describe, expect, test } from 'bun:test';
import { buildTweetThread, linkedStatusIds, resolveEntityBounds, statusIdFromUrl } from './tweetThread';

const post = (id, reply, quote, urls = []) => ({
  id, in_reply_to_status_id: reply, quoted_status_id: quote,
  entities: { urls: urls.map(expanded_url => ({ expanded_url })) },
});

describe('buildTweetThread', () => {
  test('keeps reply forks in their own branches and embeds a quote of an earlier reply', () => {
    const tweets = [
      post('root'), post('question', 'root'), post('answer', 'question'),
      post('followup', 'answer'), post('other-branch', 'answer'),
      post('response', 'other-branch'), post('last', 'response', 'followup'),
    ];
    const view = buildTweetThread(tweets, 'last');
    expect(view.rows.map(row => [row.tweet.id, row.depth])).toEqual([
      ['root', 0], ['question', 1], ['answer', 2], ['followup', 3],
      ['other-branch', 3], ['response', 4], ['last', 5],
    ]);
    expect(view.extra).toEqual([]);
    expect(view.mainIds.has('followup')).toBe(true);
    const fork = view.tree.children[0].children[0];
    expect(fork.tweet.id).toBe('answer');
    expect(fork.children.map(node => node.tweet.id)).toEqual(['followup', 'other-branch']);
    expect(fork.children[1].children[0].children[0].tweet.id).toBe('last');
  });

  test('keeps quoted and linked posts out of the reply timeline', () => {
    const tweets = [
      post('quoted'), post('123'),
      post('seed', null, 'quoted', ['https://x.com/user/status/123']),
    ];
    const view = buildTweetThread(tweets, 'seed');
    expect(view.rows.map(row => row.tweet.id)).toEqual(['seed']);
    expect(view.extra).toEqual([]);
    expect(view.byId.has('quoted')).toBe(true);
  });

  test('keeps a local destination for quotes beyond the card nesting limit', () => {
    const tweets = Array.from({ length: 10 }, (_, index) =>
      post(String(index + 1), null, index < 9 ? String(index + 2) : null)
    );
    const view = buildTweetThread(tweets, '1');
    expect(view.rows.map(row => row.tweet.id)).toEqual(['1']);
    expect(view.extra.map(tweet => tweet.id)).toEqual(['6']);
    expect(view.extraIds.has('6')).toBe(true);
  });

  test('keeps an archived retweet source with its retweet', () => {
    const retweet = { ...post('retweet'), retweeted_status: { id: 'source' } };
    const view = buildTweetThread([post('source'), retweet], 'retweet');
    expect(view.rows.map(row => row.tweet.id)).toEqual(['retweet']);
    expect(view.extra).toEqual([]);
    expect(view.byId.has('source')).toBe(true);
  });

  test('retains disconnected posts when relationship data is incomplete', () => {
    const view = buildTweetThread([post('orphan'), post('seed', 'missing')], 'seed');
    expect(view.rows.map(row => row.tweet.id)).toEqual(['seed']);
    expect(view.extra.map(tweet => tweet.id)).toEqual(['orphan']);
  });

  test('handles cyclic references without looping', () => {
    const view = buildTweetThread([post('a', 'b', 'b'), post('b', 'a', 'a')], 'a');
    expect(view.rows).toHaveLength(2);
  });
});

test('statusIdFromUrl accepts only X/Twitter post URLs', () => {
  expect(statusIdFromUrl('https://x.com/name/status/123?x=1')).toBe('123');
  expect(statusIdFromUrl('https://mobile.twitter.com/i/status/456')).toBe('456');
  expect(statusIdFromUrl('https://notx.com/name/status/123')).toBeNull();
  expect(statusIdFromUrl('https://x.com/name')).toBeNull();
});

test('linkedStatusIds finds entity and bare post links once', () => {
  expect(linkedStatusIds({
    full_text: 'See https://x.com/a/status/123 and https://twitter.com/b/status/456.',
    entities: { urls: [{ expanded_url: 'https://x.com/a/status/123' }] },
  })).toEqual(['123', '456']);
});

test('tweet entity offsets align after a leading emoji', () => {
  const text = '🫡 https://t.co/zpuXCIpS6x';
  expect(resolveEntityBounds(
    { indices: [2, 25] }, text, 'https://t.co/zpuXCIpS6x'
  )).toEqual({ s: 3, e: 26 });
});

test('tweet entity offsets preserve matching UTF-16 positions', () => {
  const text = 'hello https://t.co/abc';
  expect(resolveEntityBounds(
    { fromIndex: 6, toIndex: 22 }, text, 'https://t.co/abc'
  )).toEqual({ s: 6, e: 22 });
});
