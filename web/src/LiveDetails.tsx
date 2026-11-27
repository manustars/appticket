// The live details of an asset: what the add-on it comes from says of it now, asked by the core
// when the page asks, beside the asset. They come after the assets themselves, and an add-on that
// did not answer in time shows them as unavailable; nothing else on the page waits for them.
import type { AssetLiveDetails } from './api/client'
import { addonName, inLanguage } from './addon'
import { useI18n } from './i18n/context'

/**
 * The live details of one asset, from each add-on asked. `asking` names the add-on being asked
 * while the answer has not come yet.
 */
export function LiveDetails({ details, asking }: {
  details: AssetLiveDetails[] | undefined
  asking?: string | null
}) {
  const { t, locale } = useI18n()
  if (!details) {
    return asking ? <div className="live-details muted">{t('liveDetails.asking', { addon: addonName(asking) })}</div> : null
  }
  if (details.length === 0) return null
  return (
    <div className="live-details">
      {details.map((d) => {
        const addon = addonName(d.addon)
        if (!d.details) {
          return (
            <div key={d.addon} className="live">
              <span className="marker unavailable">{t('liveDetails.unavailable', { addon })}</span>
            </div>
          )
        }
        const { state, fields, links } = d.details
        return (
          <div key={d.addon} className="live">
            <span className={`marker live-${state}`}>
              {addon}: {t(`liveDetails.${state}`)}
            </span>
            {fields.map((f, i) => (
              <span key={i} className="live-field">
                <span className="muted">{inLanguage(f.label, locale)}:</span> {f.value}
              </span>
            ))}
            {links.map((l, i) => (
              <a key={i} href={l.url} target="_blank" rel="noopener noreferrer">{inLanguage(l.label, locale)}</a>
            ))}
          </div>
        )
      })}
    </div>
  )
}
