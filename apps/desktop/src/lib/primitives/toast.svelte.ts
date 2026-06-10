/**
 * The sanctioned-toast queue. The closed kind enum IS the §7.5 guardrail:
 * exactly three things may ever toast — (1) retraction committed (with an
 * Undo action that RE-STATES, DECISIONS E4), (2) redaction completed,
 * (3) a pending offline-volume redaction later completing on mount.
 * Nothing else in the application may toast; a fourth kind is a spec
 * change. Fed exclusively by the inspector slice (Stage C).
 *
 * Auto-dismiss timing lives in ToastHost.svelte so this queue stays a
 * pure, fake-timer-free unit (toast.test.ts).
 */

export type ToastKind = "retracted" | "redacted" | "offline-redaction-complete";

export interface ToastAction {
  label: string;
  run: () => void;
}

export interface Toast {
  id: number;
  kind: ToastKind;
  text: string;
  action?: ToastAction;
}

export const TOAST_DISMISS_MS = 5000;

export class ToastQueue {
  items = $state<Toast[]>([]);
  #nextId = 1;

  toast(kind: ToastKind, text: string, action?: ToastAction): number {
    const id = this.#nextId++;
    this.items = [...this.items, { id, kind, text, action }];
    return id;
  }

  dismiss(id: number) {
    this.items = this.items.filter((t) => t.id !== id);
  }

  /** Run a toast's action (Undo) and dismiss it. */
  act(id: number) {
    const t = this.items.find((x) => x.id === id);
    t?.action?.run();
    this.dismiss(id);
  }
}

export const toasts = new ToastQueue();
