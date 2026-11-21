/** An add-on's name as a name: `assistant` reads as "Assistant". */
export const addonName = (name: string) => name.charAt(0).toUpperCase() + name.slice(1)

/**
 * An add-on's rationale in the agent's language: that language, else English, else the first one
 * the add-on gave, else nothing. It is shown as the add-on wrote it, never translated here.
 */
export function rationaleIn(rationale: Readonly<Record<string, string>>, locale: string): string | null {
  const given = (language: string) => rationale[language]?.trim() || null
  return given(locale) ?? given('en') ?? Object.keys(rationale).map(given).find((r) => r !== null) ?? null
}
