import { useI18n } from './i18n/context'

/** Marks a requester who has left their customer, after their name or address in a list. */
export function DepartedMarker({ from }: { from?: string | null }) {
  const { t } = useI18n()
  return (
    <> <span className="marker departed">{from ? t('marker.departedFrom', { customer: from }) : t('marker.departed')}</span></>
  )
}
