// Which locale the UI speaks: the agent's own choice if they made one, else the browser's.
import { en } from './en'
import { it } from './it'
import type { Catalog } from './catalog'

export const catalogs = { en, it } satisfies Record<string, Catalog>
export type Locale = keyof typeof catalogs
export const locales = Object.keys(catalogs) as Locale[]

const storageKey = 'hd.locale'

const isLocale = (value: unknown): value is Locale =>
  typeof value === 'string' && Object.hasOwn(catalogs, value)

const primary = (tag: string) => tag.toLowerCase().split('-')[0]

function browserTags(): readonly string[] {
  return navigator.languages?.length ? navigator.languages : [navigator.language]
}

/** The stored choice; storage can be missing or refuse access (private windows, blocked data). */
function storedLocale(): Locale | null {
  try {
    const value = localStorage.getItem(storageKey)
    return isLocale(value) ? value : null
  } catch {
    return null
  }
}

export function storeLocale(locale: Locale) {
  try {
    localStorage.setItem(storageKey, locale)
  } catch {
    // Not remembered; the choice still holds until the page is reloaded.
  }
}

/** The stored choice, else the first of the browser's languages the UI speaks, else English. */
export function initialLocale(): Locale {
  const stored = storedLocale()
  if (stored) return stored
  for (const tag of browserTags()) {
    const language = primary(tag)
    if (isLocale(language)) return language
  }
  return 'en'
}

/**
 * The locale dates and numbers are formatted in: the browser's own tag for the UI's language, so
 * that an agent with en-GB gets British dates, else the UI's language itself.
 */
export function formattingLocale(locale: Locale): string {
  return browserTags().find((tag) => primary(tag) === locale) ?? locale
}

/** Makes the document say which language it is in. */
export function applyLocale(locale: Locale) {
  document.documentElement.lang = locale
  document.title = catalogs[locale]['app.title']
}
