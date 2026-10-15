/** A small padlock, for rows in folders this account can't open. */
export function LockGlyph() {
  return (
    <svg
      class="lock-glyph"
      width="12"
      height="12"
      viewBox="0 0 12 12"
      fill="none"
      stroke="currentColor"
      stroke-width="1.2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M2.5 5.5h7v5h-7ZM4 5.5V4a2 2 0 0 1 4 0v1.5" />
    </svg>
  );
}
