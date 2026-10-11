// A thread capture contains the requested post, its reply ancestors, and often
// extra posts fetched only because they were quoted. Archive order is not a
// conversation order, so keep reply and quote relationships separate.
export function buildTweetThread(tweets, requestedId) {
  const byId = new Map();
  tweets.forEach(tweet => {
    if (tweet?.id && !byId.has(String(tweet.id))) byId.set(String(tweet.id), tweet);
  });

  const seed = byId.get(String(requestedId)) || [...byId.values()].at(-1);
  if (!seed) return { tree: null, rows: [], byId, mainIds: new Set(), extra: [] };

  const children = new Map();
  for (const tweet of byId.values()) {
    const parentId = String(tweet.in_reply_to_status_id || '');
    if (parentId && parentId !== String(tweet.id) && byId.has(parentId)) {
      if (!children.has(parentId)) children.set(parentId, []);
      children.get(parentId).push(tweet);
    }
  }

  // Walk towards the earliest archived reply ancestor, then down every branch
  // in that conversation. Quoted posts from elsewhere stay inside quote cards.
  let root = seed;
  const ancestors = new Set([String(seed.id)]);
  while (byId.has(String(root.in_reply_to_status_id || '')) &&
         !ancestors.has(String(root.in_reply_to_status_id))) {
    root = byId.get(String(root.in_reply_to_status_id));
    ancestors.add(String(root.id));
  }

  const rows = [];
  const mainIds = new Set();
  function visit(tweet, depth) {
    const id = String(tweet.id);
    if (mainIds.has(id)) return null;
    mainIds.add(id);
    rows.push({ tweet, depth });
    return {
      tweet,
      children: (children.get(id) || []).map(child => visit(child, depth + 1)).filter(Boolean),
    };
  }
  const tree = visit(root, 0);

  const referenced = new Set();
  function markReferences(tweet) {
    const ids = [String(tweet.quoted_status_id || '')];
    ids.push(...linkedStatusIds(tweet));
    for (const id of ids) {
      if (!id || !byId.has(id) || referenced.has(id)) continue;
      referenced.add(id);
      markReferences(byId.get(id));
    }
  }
  rows.forEach(({ tweet }) => markReferences(tweet));

  // Keep any disconnected archived posts accessible, even if the scraper
  // returned incomplete relationship fields.
  const extra = [...byId.values()].filter(tweet =>
    !mainIds.has(String(tweet.id)) && !referenced.has(String(tweet.id))
  );
  return { tree, rows, byId, mainIds, extra };
}

export function statusIdFromUrl(value) {
  try {
    const url = new URL(value);
    if (!/^(?:www\.|mobile\.)?(?:x\.com|twitter\.com)$/i.test(url.hostname)) return null;
    return url.pathname.match(/\/(?:status|statuses)\/(\d+)(?:\/|$)/)?.[1] || null;
  } catch {
    return null;
  }
}

export function linkedStatusIds(tweet) {
  const urls = (tweet.entities?.urls || []).map(url => url.expanded_url || url.url);
  urls.push(...((tweet.full_text || '').match(/https?:\/\/[^\s<>"'\]）]+/g) || []));
  return [...new Set(urls.map(url => statusIdFromUrl(url?.replace(/[.,;:!?)（）]+$/, ''))).filter(Boolean))];
}

// X offsets sometimes count Unicode code points; JS slices UTF-16 units.
export function resolveEntityBounds(ent, fullText, ...candidates) {
  const raw = ent.fromIndex != null && ent.toIndex != null
    ? { s: ent.fromIndex, e: ent.toIndex }
    : ent.indices?.length === 2
      ? { s: ent.indices[0], e: ent.indices[1] }
      : null;
  const matches = bounds => candidates.some(c => c && fullText.slice(bounds.s, bounds.e) === c);
  if (raw && matches(raw)) return raw;

  if (raw) {
    const points = Array.from(fullText);
    if (raw.s >= 0 && raw.e <= points.length) {
      const adjusted = {
        s: points.slice(0, raw.s).join('').length,
        e: points.slice(0, raw.e).join('').length,
      };
      if (matches(adjusted)) return adjusted;
    }
  }

  for (const c of candidates) {
    if (!c) continue;
    const idx = fullText.indexOf(c);
    if (idx !== -1) return { s: idx, e: idx + c.length };
  }
  return raw;
}
