import { useEffect, useRef } from 'react'
import PublicCollectionPage from './PublicCollectionPage.jsx'

// The stub must be installed synchronously in the render body — not in a
// useEffect — because PublicCollectionPage fires its own fetch in a child
// useEffect which runs before any parent useEffect. Storing the real fetch in
// a ref ensures the first render captures it correctly without re-capturing
// on subsequent renders or storing a stale reference.
function CollectionApiStub({ response, status = 200, children }) {
  const realFetchRef = useRef(null)
  if (!realFetchRef.current) {
    realFetchRef.current = window.fetch
  }
  // Synchronous assignment: in place before child useEffects fire.
  window.fetch = (url) => {
    if (typeof url === 'string' && url.includes('/collections/')) {
      return Promise.resolve(
        new Response(JSON.stringify(response), {
          status,
          headers: { 'Content-Type': 'application/json' },
        })
      )
    }
    return realFetchRef.current(url)
  }
  useEffect(() => () => { window.fetch = realFetchRef.current }, [])
  return children
}

function makeDecorator(response, status) {
  return (Story) => (
    <CollectionApiStub response={response} status={status}>
      <Story />
    </CollectionApiStub>
  )
}

export default {
  title: 'Pages/PublicCollectionPage',
  component: PublicCollectionPage,
  parameters: { layout: 'fullscreen' },
}

const ARCHIVE_ID = 'demo'
const COLL_UID = 'coll_abc123'

const baseArgs = { archiveId: ARCHIVE_ID, collUid: COLL_UID }

const withEntriesPayload = {
  collection_uid: COLL_UID,
  name: 'Public Research Links',
  slug: 'public-research',
  default_visibility_bits: 3,
  requires_auth: false,
  created_at: '2024-01-15T10:00:00Z',
  entries: [
    {
      entry_uid: 'entry_001',
      title: 'How LLMs Work Under the Hood',
      source_kind: 'web',
      archived_at: '2024-06-01T12:34:00Z',
      collection_visibility_bits: 3,
      original_url: 'https://example.com/llms',
    },
    {
      entry_uid: 'entry_002',
      title: 'Building Reliable Distributed Systems',
      source_kind: 'web',
      archived_at: '2024-05-20T09:00:00Z',
      collection_visibility_bits: 3,
      original_url: 'https://example.com/distributed',
    },
    {
      entry_uid: 'entry_003',
      title: null,
      source_kind: 'youtube',
      archived_at: '2024-04-10T18:00:00Z',
      collection_visibility_bits: 3,
      original_url: null,
    },
  ],
}

export const WithEntries = {
  args: baseArgs,
  decorators: [makeDecorator(withEntriesPayload, 200)],
}

export const Empty = {
  args: baseArgs,
  decorators: [makeDecorator({ ...withEntriesPayload, entries: [] }, 200)],
}

export const LoadError = {
  args: baseArgs,
  decorators: [makeDecorator({ message: 'collection not found' }, 404)],
}
