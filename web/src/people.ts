import type { I18n } from './i18n/translate'

/**
 * What stands for a requester in lists and titles: their name, else their first address. A departed
 * requester can have neither, once their addresses passed to the requester they became elsewhere.
 */
export const requesterLabel = (t: I18n['t'], name: string | null | undefined, address: string | null | undefined) =>
  name || address || t('requester.unnamed')
