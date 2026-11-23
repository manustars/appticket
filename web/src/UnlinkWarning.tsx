// The links a move to another customer removes, shown before the agent confirms it: assets belong
// to the old customer, and services the new customer does not use do not fit it.
import type { components } from './api/schema'
import { useI18n } from './i18n/context'

type Schemas = components['schemas']

interface Props {
  /** The customer the tickets leave, whose assets they are. */
  from: string
  /** The customer the tickets go to. */
  to: string
  assets: Schemas['LinkedAsset'][]
  /** The linked services the new customer does not use. */
  services: Schemas['LinkedService'][]
  /** What the list is unlinked from, such as "Moving unlinks from this ticket:". */
  heading: string
}

/** Lists what moving unlinks; nothing when nothing is. */
export function UnlinkWarning({ from, to, assets, services, heading }: Props) {
  const { t } = useI18n()
  if (assets.length + services.length === 0) return null
  return (
    <div className="warning" role="note">
      <p>{heading}</p>
      <ul>
        {assets.map((a) => (
          <li key={a.id}>{t('move.unlinkAsset', { name: a.name, kind: a.kind, customer: from })}</li>
        ))}
        {services.map((s) => (
          <li key={s.id}>{t('move.unlinkService', { name: s.name, customer: to })}</li>
        ))}
      </ul>
    </div>
  )
}
