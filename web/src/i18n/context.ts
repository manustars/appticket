// How components reach the current locale's messages and formats.
import { createContext, useContext } from 'react'
import type { I18n } from './translate'

export const I18nContext = createContext<I18n | null>(null)

export function useI18n(): I18n {
  const i18n = useContext(I18nContext)
  if (!i18n) throw new Error('useI18n needs an I18nProvider above it')
  return i18n
}
