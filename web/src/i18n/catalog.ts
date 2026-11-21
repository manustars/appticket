// The shape of a catalog and what a message needs from its caller, from the types that
// scripts/i18n.mjs generates out of locales/en.json.
import type { ReactNode } from 'react'
import type { Messages } from './catalog.generated'

export type Key = keyof Messages

/** Plural forms by Intl.PluralRules category; `other` is the fallback. */
export type Plural = { readonly other: string } & { readonly [C in Intl.LDMLPluralRule]?: string }

/** A catalog: every English key, a string where English has one, plural forms where it has them. */
export type Catalog = { readonly [K in Key]: Messages[K]['plural'] extends true ? Plural : string }

/** The `{placeholder}` names of a message, plus `count` for a plural. */
export type PlaceholderOf<K extends Key> = Messages[K]['placeholders']
/** The `<tag>` names of a message. */
export type TagOf<K extends Key> = Messages[K]['tags']

/** What `t` needs: nothing for a plain message, its placeholders' values otherwise. */
export type Vars<K extends Key> = [PlaceholderOf<K>] extends [never]
  ? []
  : [Record<PlaceholderOf<K>, string | number>]

/** What `rich` needs: an element or text per placeholder, a wrapper per tag. */
export type RichVars<K extends Key> = Record<PlaceholderOf<K>, ReactNode>
  & Record<TagOf<K>, (inner: ReactNode) => ReactNode>
