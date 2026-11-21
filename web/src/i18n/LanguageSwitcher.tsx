import { useI18n } from './context'
import { languages, locales, type Locale } from './locale'

export function LanguageSwitcher() {
  const { locale, setLocale, t } = useI18n()
  return (
    <select className="language" aria-label={t('language.label')} value={locale}
      onChange={(e) => setLocale(e.target.value as Locale)}>
      {locales.map((l) => (
        <option key={l} value={l} lang={l}>{languages[l].name}</option>
      ))}
    </select>
  )
}
