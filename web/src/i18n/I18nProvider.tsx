import { useMemo, useState, type ReactNode } from 'react'
import { I18nContext } from './context'
import { applyLocale, choose, type Loaded, type Locale } from './locale'
import { createI18n } from './translate'

/**
 * Holds the language for the whole UI, starting from one already loaded. Changing it loads the
 * other catalog, then re-renders in place, keeping what was typed.
 */
export function I18nProvider({ initial, children }: { initial: Loaded; children: ReactNode }) {
  const [loaded, setLoaded] = useState(initial)

  const i18n = useMemo(() => createI18n(loaded, async (next: Locale) => {
    const language = await choose(next)
    if (!language) return
    applyLocale(language)
    setLoaded(language)
  }), [loaded])

  return <I18nContext value={i18n}>{children}</I18nContext>
}
