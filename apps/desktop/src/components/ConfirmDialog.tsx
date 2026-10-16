import { Show, createSignal, onCleanup, onMount } from "solid-js";

/** What a confirmation asks. */
export interface Confirmation {
  title: string;
  message: string;
  /** The button that goes ahead, such as "Delete". */
  action: string;
  /** The action destroys data: its button is red. */
  danger?: boolean;
}

const [asking, setAsking] = createSignal<{ request: Confirmation; resolve: (yes: boolean) => void } | null>(null);

/** Asks before going ahead. Resolves to whether the user said yes. */
export function confirmAction(request: Confirmation): Promise<boolean> {
  asking()?.resolve(false);
  return new Promise((resolve) => setAsking({ request, resolve }));
}

/** Whether a confirmation is showing. */
export const confirming = () => asking() !== null;

/**
 * The confirmation dialog, while one is asked. Enter goes ahead, Escape or a click outside
 * cancels; keys never reach the window behind it, where Delete deletes.
 */
export function ConfirmDialog(props: { onClose?: () => void }) {
  const answer = (yes: boolean) => {
    const open = asking();
    setAsking(null);
    open?.resolve(yes);
    props.onClose?.();
  };
  return (
    <Show when={asking()}>
      {(open) => <ConfirmPanel request={open().request} onAnswer={answer} />}
    </Show>
  );
}

function ConfirmPanel(props: { request: Confirmation; onAnswer: (yes: boolean) => void }) {
  let panel!: HTMLDivElement;
  let accept!: HTMLButtonElement;
  onMount(() => {
    accept.focus();
    const guard = (event: KeyboardEvent) => {
      event.stopPropagation();
      if (event.key === "Escape") {
        event.preventDefault();
        props.onAnswer(false);
      } else if (event.key === "Enter") {
        event.preventDefault();
        const focused = document.activeElement;
        props.onAnswer(!(focused instanceof HTMLButtonElement && focused.dataset.answer === "no"));
      } else if (event.key === "Tab") {
        event.preventDefault();
        const buttons = [...panel.querySelectorAll<HTMLButtonElement>("button")];
        const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
        buttons[(at + (event.shiftKey ? buttons.length - 1 : 1)) % buttons.length]?.focus();
      } else if (event.key !== " ") {
        event.preventDefault();
      }
    };
    // Captured, before anything else in the window sees the key.
    window.addEventListener("keydown", guard, true);
    onCleanup(() => window.removeEventListener("keydown", guard, true));
  });
  return (
    <div
      class="dialog-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) props.onAnswer(false);
      }}
    >
      <div ref={panel} class="dialog confirm-dialog" role="alertdialog" aria-modal="true" aria-label={props.request.title}>
        <h2 class="dialog-title">{props.request.title}</h2>
        <p class="confirm-message">{props.request.message}</p>
        <div class="dialog-footer">
          <span class="confirm-spacer" />
          <button ref={accept} class="dialog-button primary" classList={{ danger: props.request.danger }} data-answer="yes" onClick={() => props.onAnswer(true)}>
            {props.request.action}
          </button>
          <button class="dialog-button" data-answer="no" onClick={() => props.onAnswer(false)}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
