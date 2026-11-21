// Which language the UI speaks: the agent's own choice if they made one, else the browser's.
import type { Catalog } from './catalog'

/**
 * The languages the UI speaks, each named in itself so that an agent finds their own whatever the
 * UI speaks now. A catalog is loaded only when its language is chosen. To add a language, add its
 * locales/<code>.json and one line here.
 */
export const languages = {
  en: { name: 'English', load: () => import('./locales/en.json') },
  it: { name: 'Italiano', load: () => import('./locales/it.json') },
} satisfies Record<string, { name: string; load: () => Promise<{ default: Catalog }> }>

export type Locale = keyof typeof languages
export const locales = Object.keys(languages) as Locale[]
const fallback: Locale = 'en'

const storageKey = 'hd.locale'

const isLocale = (value: unknown): value is Locale =>
  typeof value === 'string' && Object.hasOwn(languages, value)

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

function storeLocale(locale: Locale) {
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
  return fallback
}

/** A language with its catalog. */
export interface Loaded {
  locale: Locale
  catalog: Catalog
}

/** Fetches a language's catalog; English if that one cannot be had. */
export async function load(locale: Locale): Promise<Loaded> {
  try {
    return { locale, catalog: (await languages[locale].load()).default }
  } catch (error) {
    if (locale === fallback) throw error
    return { locale: fallback, catalog: (await languages[fallback].load()).default }
  }
}

let lastChoice = 0

/** Remembers and loads the language the agent chose; null when a later choice overtook it. */
export async function choose(locale: Locale): Promise<Loaded | null> {
  const mine = ++lastChoice
  storeLocale(locale)
  const loaded = await load(locale)
  return mine === lastChoice ? loaded : null
}

/**
 * The locale dates and numbers are formatted in: the browser's own tag for the UI's language, so
 * that an agent with en-GB or en-US gets their own dates, else a default variant of the language.
 */
export function formattingLocale(locale: Locale): string {
  return browserTags().find((tag) => primary(tag) === locale) ?? defaultFormatting[locale] ?? locale
}

/**
 * How a language formats dates when the browser does not say which variant the agent uses. Plain
 * "en" would be US style (month first, 12-hour); day first and 24-hour is the likelier reading for
 * an agent whose browser speaks another language.
 */
const defaultFormatting: Partial<Record<Locale, string>> = { en: 'en-GB' }

/** Makes the document say which language it is in. */
export function applyLocale({ locale, catalog }: Loaded) {
  document.documentElement.lang = locale
  document.title = catalog['app.title']
}
