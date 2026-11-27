// A customer's assets, with their serial number, site and holder, and where each is: an asset whose
// kind moves with its holder is at its holder's site while it has a holder; any other asset is at
// its own site.
import { useCallback, useId, useState, type FormEvent } from 'react'
import {
  acceptAssetLinkProposal,
  createAsset,
  discardAssetLinkProposal,
  listAssetKinds,
  listAssetLinkProposals,
  listCustomerAssetLiveDetails,
  listCustomerAssets,
  retireAsset,
  updateAsset,
  type Asset,
  type AssetLinkProposal,
  type CustomerDetail,
} from './api/client'
import { useI18n } from './i18n/context'
import { liveDetailsByAsset } from './addon'
import { LiveDetails } from './LiveDetails'
import { requesterLabel } from './people'
import { Problem } from './Problem'
import { adminHref, requesterHref } from './route'
import { useAction, useLoad } from './useAction'

const none = <span className="muted">—</span>

/** The site the asset is at, following its kind, and whether that is its holder's. */
export function WhereItIs({ asset: a }: { asset: Asset }) {
  const { t } = useI18n()
  return (
    <>
      {a.at_site?.name ?? none}
      {a.moves_with_holder && a.holder && <span className="muted"> · {t('assets.withHolder')}</span>}
    </>
  )
}

/**
 * The add-on an asset comes from, when one supplies it, and the other add-ons linked to it, each
 * with its reference there.
 */
export function AssetSource({ asset: a }: { asset: Asset }) {
  const { t } = useI18n()
  return (
    <>
      {a.source_addon && (
        <>
          {' '}
          <span className="marker source" title={a.reference ?? undefined}>
            {t('assets.fromAddon', { addon: a.source_addon })}
          </span>
        </>
      )}
      {a.linked_addons.map((l) => (
        <span key={l.addon}>
          {' '}
          <span className="marker linked" title={l.reference}>
            {t('assets.linkedAddon', { addon: l.addon })}
          </span>
        </span>
      ))}
    </>
  )
}

/** The assets a requester holds, as their page lists them. */
export function HeldAssets({ assets }: { assets: Asset[] }) {
  const { t } = useI18n()
  if (assets.length === 0) return <p className="empty">{t('assets.noneHeld')}</p>
  return (
    <table className="data">
      <thead>
        <tr>
          <th>{t('assets.kind')}</th><th>{t('assets.name')}</th>
          <th>{t('assets.serialNumber')}</th><th>{t('assets.whereItIs')}</th>
        </tr>
      </thead>
      <tbody>
        {assets.map((a) => (
          <tr key={a.id}>
            <td>{a.kind}</td>
            <td>{a.name}<AssetSource asset={a} /></td>
            <td>{a.serial_number ?? none}</td>
            <td><WhereItIs asset={a} /></td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

/** What is picked and typed for the asset being edited. */
interface Draft {
  id: string
  /** The add-on it comes from: only that add-on changes its kind, name, serial number and site. */
  source: string | null
  kindId: string
  name: string
  serialNumber: string
  siteId: string
  holderId: string
}

const draftOf = (a: Asset): Draft => ({
  id: a.id,
  source: a.source_addon ?? null,
  kindId: a.kind_id,
  name: a.name,
  serialNumber: a.serial_number ?? '',
  siteId: a.site?.id ?? '',
  holderId: a.holder?.id ?? '',
})

const loadKinds = () => listAssetKinds()

interface Props {
  customer: CustomerDetail
  readOnly: boolean
  onSignedOut: () => void
}

/**
 * The open link proposals on the customer's assets: an add-on would create an asset with the same
 * serial number or name as one the customer has, and the core asked the agents instead. Every agent
 * sees them; those who manage the catalog accept them, linking the add-on to that asset, or discard
 * them, and the add-on then creates its own asset.
 */
function LinkProposals({ proposals, readOnly, busy, onAccept, onDiscard }: {
  proposals: AssetLinkProposal[]
  readOnly: boolean
  busy: boolean
  onAccept: (p: AssetLinkProposal) => void
  onDiscard: (p: AssetLinkProposal) => void
}) {
  const { t, rich } = useI18n()
  const id = useId()
  if (proposals.length === 0) return null
  return (
    <section className="link-proposals" aria-labelledby={`${id}-title`}>
      <h3 id={`${id}-title`}>{t('assets.linkProposals')}</h3>
      <p className="hint">{t('assets.linkProposalsHint')}</p>
      <ul className="proposal-list">
        {proposals.map((p) => (
          <li key={p.id} className="proposal">
            <span>
              {rich('assets.linkProposal', {
                addon: p.addon,
                name: p.name,
                reference: p.reference,
                asset: p.asset_name,
                strong: (inner) => <strong>{inner}</strong>,
              })}
              {' '}
              <span className="muted">
                · {p.matched_by === 'serial_number'
                  ? t('assets.sameSerialNumber', { serialNumber: p.serial_number ?? '' })
                  : t('assets.sameName')}
              </span>
            </span>
            {!readOnly && (
              <div className="proposal-actions">
                <button type="button" disabled={busy}
                  aria-label={t('assets.acceptLinkNamed', { name: p.name })}
                  onClick={() => onAccept(p)}>
                  {t('assets.acceptLink')}
                </button>
                <button type="button" className="quiet" disabled={busy}
                  aria-label={t('assets.discardLinkNamed', { name: p.name })}
                  onClick={() => onDiscard(p)}>
                  {t('assets.discardLink')}
                </button>
              </div>
            )}
          </li>
        ))}
      </ul>
    </section>
  )
}

/** The customer's assets: added, corrected and retired by those who manage the catalog. */
export function CustomerAssets({ customer: c, readOnly, onSignedOut }: Props) {
  const { t, rich, formatTime } = useI18n()
  // Loaded again whenever the customer is, since retiring a site or a requester's departure
  // changes where its assets are and who holds them.
  const load = useCallback(() => listCustomerAssets(c.id), [c])
  const assets = useLoad(load, onSignedOut)
  const loadProposals = useCallback(() => listAssetLinkProposals(c.id), [c])
  const proposals = useLoad(loadProposals, onSignedOut)
  // Asked of the add-ons apart, so that the assets show at once and their live details beside
  // them when the add-ons have answered; asked again for another customer only, not each time
  // this one is reloaded.
  const loadLive = useCallback(() => listCustomerAssetLiveDetails(c.id), [c.id])
  const live = useLoad(loadLive, onSignedOut)
  const liveByAsset = liveDetailsByAsset(live.data)
  const kinds = useLoad(loadKinds, onSignedOut)
  const action = useAction(onSignedOut)
  const [editing, setEditing] = useState<Draft | null>(null)
  const [kind, setKind] = useState('')
  const [name, setName] = useState('')
  const [serialNumber, setSerialNumber] = useState('')
  const [siteId, setSiteId] = useState('')
  const id = useId()
  // An asset is put only at an active site, and given only to a requester who has not departed.
  const sites = c.sites.filter((s) => !s.retired_at)
  const holders = c.requester_list.filter((r) => !r.departed)

  function add(event: FormEvent) {
    event.preventDefault()
    if (!kind || !name.trim()) return
    const fields = {
      kind_id: kind,
      name: name.trim(),
      serial_number: serialNumber.trim() || null,
      site_id: siteId || null,
    }
    void action.run(() => createAsset(c.id, fields)).then(async (ok) => {
      if (!ok) return
      setKind('')
      setName('')
      setSerialNumber('')
      setSiteId('')
      await assets.reload()
    })
  }

  function save(event: FormEvent) {
    event.preventDefault()
    if (!editing || !editing.kindId || !editing.name.trim()) return
    const fields = {
      kind_id: editing.kindId,
      name: editing.name.trim(),
      serial_number: editing.serialNumber.trim() || null,
      site_id: editing.siteId || null,
      holder_id: editing.holderId || null,
    }
    void action.run(() => updateAsset(editing.id, fields)).then(async (ok) => {
      if (!ok) return
      setEditing(null)
      await assets.reload()
    })
  }

  const kindOptions = kinds.data?.map((k) => <option key={k.id} value={k.id}>{k.name}</option>)

  function retire(a: Asset) {
    const question = a.holder
      ? t('assets.confirmRetireHeld', { name: a.name, holder: requesterLabel(t, a.holder.name, a.holder.address) })
      : t('assets.confirmRetire', { name: a.name })
    if (!window.confirm(question)) return
    void action.run(() => retireAsset(a.id)).then(async (ok) => { if (ok) await assets.reload() })
  }

  function acceptLink(p: AssetLinkProposal) {
    if (!window.confirm(t('assets.confirmAcceptLink', { addon: p.addon, asset: p.asset_name, reference: p.reference }))) return
    void action.run(() => acceptAssetLinkProposal(p.id)).then(async (ok) => {
      if (ok) await Promise.all([proposals.reload(), assets.reload()])
    })
  }

  function discardLink(p: AssetLinkProposal) {
    if (!window.confirm(t('assets.confirmDiscardLink', { addon: p.addon, name: p.name, asset: p.asset_name }))) return
    void action.run(() => discardAssetLinkProposal(p.id)).then(async (ok) => { if (ok) await proposals.reload() })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('assets.title')}</h2>
      <p className="hint">{t('assets.hint')}</p>
      <Problem error={assets.error} />
      <Problem error={kinds.error} />
      <Problem error={proposals.error} />
      <Problem error={live.error} />
      {proposals.data && (
        <LinkProposals proposals={proposals.data} readOnly={readOnly} busy={action.busy}
          onAccept={acceptLink} onDiscard={discardLink} />
      )}
      {assets.data?.length === 0 && <p className="empty">{t('assets.empty')}</p>}
      {assets.data && assets.data.length > 0 && (
        // The row being edited submits the form below by its id, so that the table sits in no form.
        <table className="data assets">
          <thead>
            <tr>
              <th>{t('assets.kind')}</th><th>{t('assets.name')}</th><th>{t('assets.serialNumber')}</th>
              <th>{t('assets.whereItIs')}</th><th>{t('assets.holder')}</th>
              <th><span className="visually-hidden">{t('table.actions')}</span></th>
            </tr>
          </thead>
          <tbody>
            {assets.data.map((a) => editing?.id === a.id ? (
              // Its own key: reusing the shown row would turn the clicked Edit button into Save,
              // and the click would then submit the form.
              <tr key={`${a.id}-edit`}>
                {editing.source ? (
                  // What its source supplied is changed in the add-on's system, not here.
                  <>
                    <td>{a.kind}</td>
                    <td>{a.name}<AssetSource asset={a} /></td>
                    <td>{a.serial_number ?? none}</td>
                    <td>{a.site?.name ?? none}</td>
                  </>
                ) : (
                  <>
                    <td>
                      <select form={`${id}-edit`} aria-label={t('assets.kind')} required value={editing.kindId}
                        onChange={(e) => setEditing({ ...editing, kindId: e.target.value })}>
                        {kindOptions}
                      </select>
                    </td>
                    <td>
                      <input form={`${id}-edit`} aria-label={t('assets.name')} required value={editing.name}
                        onChange={(e) => setEditing({ ...editing, name: e.target.value })} />
                    </td>
                    <td>
                      <input form={`${id}-edit`} aria-label={t('assets.serialNumber')} value={editing.serialNumber}
                        onChange={(e) => setEditing({ ...editing, serialNumber: e.target.value })} />
                    </td>
                    <td>
                      <select form={`${id}-edit`} aria-label={t('assets.ownSite')} value={editing.siteId}
                        onChange={(e) => setEditing({ ...editing, siteId: e.target.value })}>
                        <option value="">{t('assets.noSite')}</option>
                        {sites.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
                      </select>
                    </td>
                  </>
                )}
                <td>
                  <select form={`${id}-edit`} aria-label={t('assets.holder')} value={editing.holderId}
                    onChange={(e) => setEditing({ ...editing, holderId: e.target.value })}>
                    <option value="">{t('assets.noHolder')}</option>
                    {holders.map((r) => (
                      <option key={r.id} value={r.id}>{requesterLabel(t, r.name, r.addresses[0])}</option>
                    ))}
                  </select>
                </td>
                <td className="row-actions">
                  <button type="submit" form={`${id}-edit`} disabled={action.busy}>{t('action.save')}</button>
                  <button type="button" className="quiet" onClick={() => { setEditing(null); action.clear() }}>
                    {t('action.cancel')}
                  </button>
                </td>
              </tr>
            ) : (
              <tr key={a.id} className={a.retired_at ? 'retired' : ''}>
                <td>{a.kind}</td>
                <td>
                  {a.name}
                  <AssetSource asset={a} />
                  {a.retired_at && (
                    <> <span className="marker retired">{t('assets.retired', { date: formatTime(a.retired_at) })}</span></>
                  )}
                  {!a.retired_at && (
                    <LiveDetails details={liveByAsset?.get(a.id) ?? (liveByAsset ? [] : undefined)}
                      asking={live.error ? null : a.source_addon} />
                  )}
                </td>
                <td>{a.serial_number ?? none}</td>
                <td><WhereItIs asset={a} /></td>
                <td>
                  {a.holder
                    ? <a href={requesterHref(a.holder.id)}>{requesterLabel(t, a.holder.name, a.holder.address)}</a>
                    : none}
                </td>
                <td className="row-actions">
                  {!readOnly && !a.retired_at && (
                    <>
                      {a.source_addon ? (
                        // Only its holder is the agents' to change; its source retires it.
                        <button type="button" className="link" disabled={action.busy}
                          aria-label={t('assets.setHolderNamed', { name: a.name })}
                          onClick={() => { setEditing(draftOf(a)); action.clear() }}>
                          {t('assets.setHolder')}
                        </button>
                      ) : (
                        <>
                          <button type="button" className="link" disabled={action.busy}
                            aria-label={t('assets.editNamed', { name: a.name })}
                            onClick={() => { setEditing(draftOf(a)); action.clear() }}>
                            {t('action.edit')}
                          </button>
                          <button type="button" className="link danger" disabled={action.busy}
                            aria-label={t('assets.retireNamed', { name: a.name })}
                            onClick={() => retire(a)}>
                            {t('assets.retire')}
                          </button>
                        </>
                      )}
                    </>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {assets.data?.some((a) => a.source_addon) && <p className="hint">{t('assets.suppliedHint')}</p>}
      {assets.data?.some((a) => a.linked_addons.length > 0) && <p className="hint">{t('assets.linkedHint')}</p>}
      {editing && <form id={`${id}-edit`} onSubmit={save} />}
      {!readOnly && kinds.data?.length === 0 && (
        <p className="hint">
          {rich('assets.noKinds', { link: (inner) => <a href={adminHref('kinds')}>{inner}</a> })}
        </p>
      )}
      {!readOnly && (
        <form className="inline-form" onSubmit={add}>
          <label>
            {t('assets.kind')}
            <select required value={kind} onChange={(e) => setKind(e.target.value)}>
              <option value="" disabled>{t('assets.chooseKind')}</option>
              {kindOptions}
            </select>
          </label>
          <label>
            {t('assets.name')}
            <input required value={name} onChange={(e) => setName(e.target.value)} />
          </label>
          <label>
            {t('assets.serialNumber')}
            <input value={serialNumber} onChange={(e) => setSerialNumber(e.target.value)} />
          </label>
          <label>
            {t('assets.ownSite')}
            <select value={siteId} onChange={(e) => setSiteId(e.target.value)}>
              <option value="">{t('assets.noSite')}</option>
              {sites.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
            </select>
          </label>
          <button type="submit" disabled={action.busy || !kind || !name.trim()}>{t('assets.add')}</button>
        </form>
      )}
      <Problem error={action.error} />
    </section>
  )
}
