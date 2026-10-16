/**
 * An item's check box, as Explorer's View > Show > Item check boxes draws one: shown on the item
 * under the pointer and on selected items, in every view (parity2.css places it). A press on it
 * is the list's to handle (`onItemMouseDown` in FileList.tsx): it adds the item to the selection
 * or takes it out, as Ctrl+click does.
 */
export function ItemCheck(props: { checked: boolean }) {
  return (
    <span class="item-check" classList={{ checked: props.checked }} role="checkbox" aria-checked={props.checked} aria-label="Select">
      <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
        <rect class="item-check-box" x="0.75" y="0.75" width="12.5" height="12.5" rx="2.5" />
        <path class="item-check-mark" d="M3.6 7.3 6 9.6l4.4-5" fill="none" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </span>
  );
}
