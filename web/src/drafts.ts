// Keeps the composer's unsent text on the core, so that it survives leaving the ticket, a reload or
// another device. The core clears it when the reply or the note is sent.
import { useCallback, useEffect, useRef, useState } from 'react'
import { saveDraft, Unauthorized, type DraftMode } from './api/client'

/** How long typing pauses before the draft is saved. */
const saveDelay = 1000

export interface DraftValue {
  mode: DraftMode
  body: string
}

export type SaveState =
  | { kind: 'saving' }
  | { kind: 'saved' }
  | { kind: 'failed'; error: unknown }
  | null

const same = (a: DraftValue | null, b: DraftValue) => a !== null && a.mode === b.mode && a.body === b.body

export interface DraftKeeper {
  saveState: SaveState
  /** The agent changed the text or the mode: it is saved once typing pauses. */
  changed: (value: DraftValue) => void
  /** Saves now what is waiting, e.g. when the composer loses the focus. */
  flush: () => void
  /** The core already holds this: it was loaded, saved by accepting a proposal, or cleared by sending. */
  known: (value: DraftValue) => void
}

/** Saves the draft of ticket `number`, debounced, and once more when the ticket or the page is left. */
export function useDraftKeeper(number: number, onSignedOut: () => void): DraftKeeper {
  const [saveState, setSaveState] = useState<SaveState>(null)
  // What the core holds, as far as this page knows; null until the draft has been loaded.
  const saved = useRef<DraftValue | null>(null)
  // What waits to be saved.
  const pending = useRef<DraftValue | null>(null)
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  // Saves run one after another, so that an older text never lands after a newer one.
  const queue = useRef<Promise<void>>(Promise.resolve())

  const save = useCallback((keepalive: boolean) => {
    clearTimeout(timer.current)
    const next = pending.current
    pending.current = null
    if (!next) return
    queue.current = queue.current.then(async () => {
      if (same(saved.current, next)) return
      setSaveState({ kind: 'saving' })
      try {
        await saveDraft(number, next.mode, next.body, keepalive)
        saved.current = next
        setSaveState(next.body ? { kind: 'saved' } : null)
      } catch (error) {
        if (error instanceof Unauthorized) onSignedOut()
        else setSaveState({ kind: 'failed', error })
      }
    })
  }, [number, onSignedOut])

  const changed = useCallback((value: DraftValue) => {
    pending.current = value
    clearTimeout(timer.current)
    timer.current = setTimeout(() => save(false), saveDelay)
  }, [save])

  const flush = useCallback(() => save(false), [save])

  const known = useCallback((value: DraftValue) => {
    clearTimeout(timer.current)
    pending.current = null
    saved.current = value
    setSaveState(value.body ? { kind: 'saved' } : null)
  }, [])

  useEffect(() => {
    // Leaving the page: the request has to outlive it.
    const leave = () => save(true)
    window.addEventListener('pagehide', leave)
    return () => {
      window.removeEventListener('pagehide', leave)
      // Leaving the ticket: what was typed in the last second is saved too.
      save(false)
    }
  }, [save])

  return { saveState, changed, flush, known }
}
