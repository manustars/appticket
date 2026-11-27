import type { AssetLiveDetails } from './api/client'

/** The live details the add-ons gave of each asset, by the asset's id; null until they came. */
export function liveDetailsByAsset(
  details: AssetLiveDetails[] | null,
): ReadonlyMap<string, AssetLiveDetails[]> | null {
  if (!details) return null
  const map = new Map<string, AssetLiveDetails[]>()
  for (const d of details) map.set(d.asset_id, [...(map.get(d.asset_id) ?? []), d])
  return map
}

/** An add-on's name as a name: `assistant` reads as "Assistant". */
export const addonName = (name: string) => name.charAt(0).toUpperCase() + name.slice(1)

/**
 * What an add-on wrote per language (a proposal's rationale, a label of live details) in the
 * agent's language: that language, else English, else the first one the add-on gave, else
 * nothing. It is shown as the add-on wrote it, never translated here.
 */
export function inLanguage(texts: Readonly<Record<string, string>>, locale: string): string | null {
  const given = (language: string) => texts[language]?.trim() || null
  return given(locale) ?? given('en') ?? Object.keys(texts).map(given).find((r) => r !== null) ?? null
}
