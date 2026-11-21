// What every screen does with the core: load something and keep the latest answer, run a change
// and keep the core's refusal next to the part of the screen that asked for it.
import { useCallback, useEffect, useRef, useState } from 'react'
import { Unauthorized } from './api/client'

export interface Loaded<T> {
  /** null until the first answer arrives. */
  data: T | null
  /** The last failure, kept as it came so that it is put into words in the current language. */
  error: unknown
  reload: () => Promise<void>
}

/** Loads with `load` when it changes (keep it stable with useCallback), and on `reload`. */
export function useLoad<T>(load: () => Promise<T>, onSignedOut: () => void): Loaded<T> {
  // An answer counts only for the `load` that gave it: once `load` changes (another customer
  // chosen, say), the old answer is not shown as if it were the new one.
  const [answer, setAnswer] = useState<{ from: () => Promise<T>; value: T } | null>(null)
  const [error, setError] = useState<unknown>(null)
  // Only the latest load may update the screen: a slow answer must not overwrite a newer one.
  const latest = useRef(0)

  const reload = useCallback(async () => {
    const mine = ++latest.current
    try {
      const value = await load()
      if (mine !== latest.current) return
      setAnswer({ from: load, value })
      setError(null)
    } catch (e) {
      if (mine !== latest.current) return
      if (e instanceof Unauthorized) onSignedOut()
      else setError(e)
    }
  }, [load, onSignedOut])

  useEffect(() => {
    const first = setTimeout(reload, 0)
    return () => clearTimeout(first)
  }, [reload])

  return { data: answer && answer.from === load ? answer.value : null, error, reload }
}

export interface Action {
  busy: boolean
  /** The last refusal, or null. */
  error: unknown
  /** Runs `change`; returns whether it succeeded. A refusal is kept in `error`. */
  run: (change: () => Promise<void>) => Promise<boolean>
  clear: () => void
}

export function useAction(onSignedOut: () => void): Action {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>(null)

  const run = useCallback(async (change: () => Promise<void>) => {
    setBusy(true)
    setError(null)
    try {
      await change()
      return true
    } catch (e) {
      if (e instanceof Unauthorized) onSignedOut()
      else setError(e)
      return false
    } finally {
      setBusy(false)
    }
  }, [onSignedOut])

  const clear = useCallback(() => setError(null), [])
  return { busy, error, run, clear }
}
