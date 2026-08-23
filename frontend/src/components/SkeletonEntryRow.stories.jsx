import SkeletonEntryRow from './SkeletonEntryRow';

export default {
  component: SkeletonEntryRow,
  tags: ['autodocs'],
  parameters: { layout: 'padded' },
};

function PendingRows({ locators }) {
  return (
    <div className="entry-table">
      <div id="entries-body">
        {locators.map(locator => (
          <SkeletonEntryRow key={locator} locator={locator} />
        ))}
      </div>
    </div>
  );
}

export const Examples = {
  render: () => (
    <PendingRows
      locators={[
        'https://example.com/articles/a-small-and-useful-page',
        'https://www.youtube.com/watch?v=abc123&list=PL1234567890',
        'tweet:1891234567890123456',
      ]}
    />
  ),
};
