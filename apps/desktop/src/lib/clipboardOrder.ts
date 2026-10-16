// The order of the clipboard's writes and reads: Copy, Cut and a copy out of an archive put files
// on the clipboard through `writesClipboard`, and a paste waits for the last of them
// (`afterClipboardWrite`). lib/api.ts and lib/archivesApi.ts send their commands through these.

/**
 * The last command that puts files on the clipboard, until it is done. Commands run on a pool of
 * threads, so a paste sent while a copy was still on its way (Ctrl+C, then Ctrl+V at once) could
 * read the clipboard first and paste what was on it before; every paste waits for it.
 */
let clipboardWrite: Promise<unknown> = Promise.resolve();
/** Clipboard writes on their way, pastes waiting for them, and pastes done (development builds show them). */
let writing = 0;
let waiting = 0;
let pasted = 0;
/** Development builds: clipboard writes wait here while a test holds them, before they are sent. */
let writeGate: Promise<void> | null = null;
let openWriteGate = () => {};

/** Sends a command that puts files on the clipboard (`send`), as the write pastes wait for. */
export function writesClipboard<T>(send: () => Promise<T>): Promise<T> {
  writing++;
  const write = import.meta.env.DEV && writeGate ? writeGate.then(send) : send();
  clipboardWrite = write.then(
    () => void writing--,
    () => void writing--,
  );
  return write;
}

/** Waits for the clipboard's last write before pasting. */
export function afterClipboardWrite<T>(paste: () => Promise<T>): Promise<T> {
  waiting++;
  return clipboardWrite
    .then(() => {
      waiting--;
      return paste();
    })
    .finally(() => void pasted++);
}

if (import.meta.env.DEV) {
  Object.assign(window, {
    __kestrelClipboard: {
      /** Holds clipboard writes before they are sent, or sends those held, so a test can paste while a copy is on its way. */
      hold: (hold: boolean) => {
        if (hold && !writeGate) writeGate = new Promise((resolve) => (openWriteGate = resolve));
        if (!hold) {
          writeGate = null;
          openWriteGate();
        }
      },
      writing: () => writing,
      waiting: () => waiting,
      pasted: () => pasted,
    },
  });
}
