// The shape every catalog has, derived from the English one, and what a message needs from its
// caller.
import type { ReactNode } from 'react'
import type { en } from './en'

type En = typeof en
export type Key = keyof En

/** Plural forms by Intl.PluralRules category; `other` is the fallback. */
export type Plural = { readonly other: string } & { readonly [C in Intl.LDMLPluralRule]?: string }

/** A translation: the same keys as English, a string where English has one, plural forms where it has them. */
export type Catalog = { readonly [K in Key]: En[K] extends string ? string : Plural }

type Text<M> = M extends string ? M : M[keyof M]
type Placeholders<S> = S extends `${string}{${infer P}}${infer Rest}` ? P | Placeholders<Rest> : never
type Tags<S> = S extends `${string}<${infer T}>${infer Rest}`
  ? T extends `/${string}` ? Tags<Rest> : T | Tags<Rest>
  : never
type Counted<M> = M extends string ? never : 'count'

/** The `{placeholder}` names of a message, plus `count` for a plural. */
export type PlaceholderOf<K extends Key> = Placeholders<Text<En[K]>> | Counted<En[K]>
/** The `<tag>` names of a message. */
export type TagOf<K extends Key> = Tags<Text<En[K]>>

/** What `t` needs: nothing for a plain message, its placeholders' values otherwise. */
export type Vars<K extends Key> = [PlaceholderOf<K>] extends [never]
  ? []
  : [Record<PlaceholderOf<K>, string | number>]

/** What `rich` needs: an element or text per placeholder, a wrapper per tag. */
export type RichVars<K extends Key> = Record<PlaceholderOf<K>, ReactNode>
  & Record<TagOf<K>, (inner: ReactNode) => ReactNode>

type Names<M> = Placeholders<Text<M>> | Tags<Text<M>>
type Same<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false
/**
 * The keys whose message names other placeholders or tags than its English source; `never` when
 * there are none. `C` must keep its literal types (`as const`).
 */
export type Mismatched<C extends Catalog> = {
  [K in Key]: Same<Names<En[K]>, Names<C[K]>> extends true ? never : K
}[Key]

/** Compiles only when `T` is `never`; the error shows what `T` is instead. */
export type None<T extends never> = T
