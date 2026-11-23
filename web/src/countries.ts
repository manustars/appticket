// The countries a tax identifier can come from: the ISO 3166-1 alpha-2 codes, named in the agent's
// language by the browser.
import { formattingLocale, type Locale } from './i18n/locale'

const codes = `
  AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL BM BN BO BQ BR BS
  BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ DK DM DO DZ EC EE
  EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK HM
  HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC
  LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA
  NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW
  SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO
  TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW
`.trim().split(/\s+/)

export interface Country {
  code: string
  name: string
}

/** Names countries in the agent's language: its code when the browser has no name for it. */
function namer(locale: Locale): (code: string) => string {
  const names = new Intl.DisplayNames([formattingLocale(locale)], { type: 'region' })
  // The core keeps two capital letters, the only codes `of` accepts without throwing.
  return (code) => names.of(code) ?? code
}

/** The country's name in the agent's language. */
export function countryName(locale: Locale, code: string): string {
  return namer(locale)(code)
}

/** Every country, by name in the agent's language. */
export function countries(locale: Locale): Country[] {
  const name = namer(locale)
  const collator = new Intl.Collator(formattingLocale(locale))
  return codes
    .map((code) => ({ code, name: name(code) }))
    .sort((a, b) => collator.compare(a.name, b.name))
}
