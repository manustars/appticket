// What a move to another customer does, shown before the agent confirms it: a requester who moves
// takes all their tickets and leaves their site and the assets they hold, and links that do not
// fit the new customer are removed (assets belong to the old customer; services the new customer
// does not use).
import type { ReactNode } from 'react'
import type { MovePreview } from './api/client'
import type { components } from './api/schema'
import { useI18n } from './i18n/context'

type Schemas = components['schemas']

interface UnlinkProps {
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
export function UnlinkWarning({ from, to, assets, services, heading }: UnlinkProps) {
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

interface RequesterProps {
  preview: MovePreview
  from: string
  to: string
  /** Said first, such as how many tickets move along. */
  lead?: ReactNode
  heading: string
}

/**
 * The warnings of a requester's move: the site they leave, their being a key requester of the old
 * customer, which ends, the assets they hold, which stay with the old customer without a holder,
 * and every link the move removes.
 */
export function RequesterMoveWarnings({ preview, from, to, lead, heading }: RequesterProps) {
  const { t } = useI18n()
  const held = preview.held_assets
  return (
    <>
      {(lead || preview.site || preview.key_requester || held.length > 0) && (
        <div className="warning" role="note">
          {lead && <p>{lead}</p>}
          {preview.site && <p>{t('requester.moveLeavesSite', { site: preview.site.name, customer: from })}</p>}
          {preview.key_requester && <p>{t('requester.moveEndsKeyRequester', { customer: from })}</p>}
          {held.length > 0 && (
            <p>
              {t('requester.moveLeavesAssets', {
                count: held.length,
                assets: held.map((a) => t('move.heldAsset', { name: a.name, kind: a.kind })).join(', '),
                customer: from,
              })}
            </p>
          )}
        </div>
      )}
      <UnlinkWarning from={from} to={to} assets={preview.assets} services={preview.services} heading={heading} />
    </>
  )
}
