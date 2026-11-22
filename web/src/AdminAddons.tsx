// The add-ons the help desk has registered: switching them on and off, and how delivering events
// to them goes. Registering one stays on the server's command line, where its secrets are made.
import { useId, type ReactNode } from 'react'
import { addonName } from './addon'
import { listAddons, setAddonEnabled, type AddonRow } from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { useAction, useLoad } from './useAction'

const loadAddons = () => listAddons()

export function AdminAddons({ onSignedOut }: { onSignedOut: () => void }) {
  const { t, rich } = useI18n()
  const addons = useLoad(loadAddons, onSignedOut)
  const action = useAction(onSignedOut)
  const id = useId()

  function toggle(a: AddonRow) {
    void action.run(() => setAddonEnabled(a.name, !a.enabled)).then(() => addons.reload())
  }

  const list = (items: string[]) => (items.length ? items.join(', ') : <span className="muted">—</span>)
  return (
    <>
      <section className="panel" aria-labelledby={`${id}-title`}>
        <div className="title-row">
          <h2 id={`${id}-title`}>{t('addons.title')}</h2>
          <button type="button" className="link" disabled={action.busy} onClick={() => void addons.reload()}>
            {t('admin.refresh')}
          </button>
        </div>
        <Problem error={addons.error} />
        {addons.data?.length === 0 && <p className="empty">{t('addons.empty')}</p>}
        {addons.data && addons.data.length > 0 && (
          <table className="data">
            <thead>
              <tr>
                <th>{t('addons.name')}</th><th>{t('addons.events')}</th><th>{t('addons.scopes')}</th>
                <th className="figure">{t('addons.pending')}</th><th className="figure">{t('addons.failing')}</th>
                <th>{t('addons.switch')}</th>
              </tr>
            </thead>
            <tbody>
              {addons.data.map((a) => (
                <AddonRows key={a.name} addon={a} busy={action.busy} list={list} onToggle={() => toggle(a)} />
              ))}
            </tbody>
          </table>
        )}
        <Problem error={action.error} />
      </section>
      <p className="hint">{rich('addons.register', { code: (inner) => <code>{inner}</code> })}</p>
    </>
  )
}

/** An add-on's row, and below it why its latest delivery failed, if one did. */
function AddonRows({ addon: a, busy, list, onToggle }: {
  addon: AddonRow
  busy: boolean
  list: (items: string[]) => ReactNode
  onToggle: () => void
}) {
  const { t, formatNumber } = useI18n()
  return (
    <>
      <tr className={a.enabled ? '' : 'retired'}>
        <td>
          <strong>{addonName(a.name)}</strong>
          <div className="muted small">{a.url}</div>
        </td>
        <td className="wrap">{list(a.events)}</td>
        <td className="wrap">{list(a.scopes)}</td>
        <td className="figure">{formatNumber(a.pending)}</td>
        <td className={`figure ${a.failing > 0 ? 'error' : ''}`}>{formatNumber(a.failing)}</td>
        <td>
          <label className="switch">
            <input type="checkbox" role="switch" checked={a.enabled} disabled={busy} onChange={onToggle}
              aria-label={t('addons.switchNamed', { name: addonName(a.name) })} />
            <span>{a.enabled ? t('addons.on') : t('addons.off')}</span>
          </label>
        </td>
      </tr>
      {a.last_error && (
        <tr className="addon-error">
          <td colSpan={6}>
            <p className="error">{t('addons.lastError', { error: a.last_error })}</p>
          </td>
        </tr>
      )}
    </>
  )
}
