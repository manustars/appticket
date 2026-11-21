import { useI18n } from './context'
import { catalogs, locales, type Locale } from './locale'

/** Each language is named in itself, so an agent finds their own whatever the UI speaks now. */
export function LanguageSwitcher() {
  const { locale, setLocale, t } = useI18n()
  return (
    <select className="language" aria-label={t('language.label')} value={locale}
      onChange={(e) => setLocale(e.target.value as Locale)}>
      {locales.map((l) => (
        <option key={l} value={l} lang={l}>{catalogs[l]['language.name']}</option>
      ))}
    </select>
  )
}
