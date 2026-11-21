// Turns a catalog and a locale into the functions components use.
import { createElement, Fragment, type ReactNode } from 'react'
import { Refused } from '../api/client'
import type { Key, Plural, RichVars, Vars } from './catalog'
import { formattingLocale, type Loaded, type Locale } from './locale'

export interface I18n {
  locale: Locale
  /** Switches language once its catalog has loaded. */
  setLocale: (locale: Locale) => void
  /** The message as text, its placeholders filled in. */
  t: <K extends Key>(key: K, ...vars: Vars<K>) => string
  /** The message with elements in it: placeholders take nodes, tags take a wrapper. */
  rich: <K extends Key>(key: K, vars: RichVars<K>) => ReactNode
  formatTime: (iso: string) => string
  formatNumber: (value: number) => string
  /** What to tell the agent about a failed call: the core's own text when it refused, as it sent it. */
  errorText: (error: unknown) => string
}

// A tag with what it wraps, or a placeholder.
const token = /<(\w+)>([\s\S]*?)<\/\1>|\{(\w+)\}/g

export function createI18n({ locale, catalog }: Loaded, setLocale: (locale: Locale) => void): I18n {
  const formatting = formattingLocale(locale)
  const plurals = new Intl.PluralRules(formatting)
  const numbers = new Intl.NumberFormat(formatting)
  const dates = new Intl.DateTimeFormat(formatting, { dateStyle: 'short', timeStyle: 'short' })

  function template(key: Key, count: unknown): string {
    const message: string | Plural = catalog[key]
    if (typeof message === 'string') return message
    const category = typeof count === 'number' ? plurals.select(count) : 'other'
    return message[category] ?? message.other
  }

  function value(name: string, vars: Record<string, unknown>): unknown {
    const v = vars[name]
    if (name === 'count' && typeof v === 'number') return numbers.format(v)
    return v === undefined ? `{${name}}` : v
  }

  /** Splits a message into text and the values of its placeholders and tags. */
  function parts(text: string, vars: Record<string, unknown>): unknown[] {
    const out: unknown[] = []
    let last = 0
    for (const match of text.matchAll(token)) {
      if (match.index > last) out.push(text.slice(last, match.index))
      const [, tag, inner, name] = match
      if (tag !== undefined) {
        const wrap = vars[tag]
        const content = parts(inner, vars)
        out.push(typeof wrap === 'function' ? wrap(createElement(Fragment, null, ...(content as ReactNode[]))) : content.join(''))
      } else {
        out.push(value(name, vars))
      }
      last = match.index + match[0].length
    }
    if (last < text.length) out.push(text.slice(last))
    return out
  }

  return {
    locale,
    setLocale,
    t: (key, ...vars) => {
      const values: Record<string, unknown> = vars[0] ?? {}
      return parts(template(key, values.count), values).join('')
    },
    rich: (key, vars) => {
      const values = vars as Record<string, unknown>
      return createElement(Fragment, null, ...(parts(template(key, values.count), values) as ReactNode[]))
    },
    formatTime: (iso) => dates.format(new Date(iso)),
    formatNumber: (n) => numbers.format(n),
    errorText: (error) => (error instanceof Refused ? error.message : catalog['error.unreachable']),
  }
}
