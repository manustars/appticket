import { useMemo, useState, type ReactNode } from 'react'
import { I18nContext } from './context'
import { applyLocale, initialLocale, storeLocale, type Locale } from './locale'
import { createI18n } from './translate'

/** Holds the locale for the whole UI; changing it re-renders in place and keeps what was typed. */
export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocale] = useState<Locale>(() => {
    const locale = initialLocale()
    applyLocale(locale)
    return locale
  })

  const i18n = useMemo(() => createI18n(locale, (next: Locale) => {
    storeLocale(next)
    applyLocale(next)
    setLocale(next)
  }), [locale])

  return <I18nContext value={i18n}>{children}</I18nContext>
}
