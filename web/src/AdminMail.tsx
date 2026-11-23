// How reading and sending mail goes, what waits in the outbox, and how signing in through the
// identity provider goes. It only shows: the settings live in the server's configuration, not here.
import { useCallback, useEffect, useId, type ReactNode } from 'react'
import { getIdentityProviderState, getMailState, type Activity } from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { ticketHref } from './route'
import { useLoad } from './useAction'

const loadMail = () => getMailState()
const loadIdentityProvider = () => getIdentityProviderState()

export function AdminMail({ onSignedOut }: { onSignedOut: () => void }) {
  const { t, formatTime, formatNumber } = useI18n()
  const mail = useLoad(loadMail, onSignedOut)
  const provider = useLoad(loadIdentityProvider, onSignedOut)
  const { reload: reloadMail } = mail
  const { reload: reloadProvider } = provider
  const id = useId()

  const reload = useCallback(async () => {
    await Promise.all([reloadMail(), reloadProvider()])
  }, [reloadMail, reloadProvider])

  useEffect(() => {
    // Mail is read and sent every few seconds; the screen follows while it is open.
    const timer = setInterval(() => void reload(), 10_000)
    return () => clearInterval(timer)
  }, [reload])

  const m = mail.data
  const p = provider.data
  return (
    <>
      <div className="title-row">
        <p className="hint">{t('mail.hint')}</p>
        <button type="button" className="link" onClick={() => void reload()}>{t('admin.refresh')}</button>
      </div>
      <Problem error={mail.error} />
      {m && (
        <>
          <div className="mail-sides">
            <MailActivity title={t('mail.reading')} activity={m.reading ?? null} notConfigured={t('mail.readingOff')} />
            <MailActivity title={t('mail.sending')} activity={m.sending ?? null} notConfigured={t('mail.sendingOff')}>
              <div>
                <dt>{t('mail.from')}</dt>
                <dd>{m.from_address ?? <span className="muted">{t('mail.noFrom')}</span>}</dd>
              </div>
            </MailActivity>
          </div>
          <section className="panel" aria-labelledby={`${id}-outbox`}>
            <h2 id={`${id}-outbox`}>{t('mail.outbox')}</h2>
            <p>
              {m.outbox.waiting === 0
                ? t('mail.nothingWaiting')
                : m.outbox.oldest_waiting_at
                  ? t('mail.waitingSince', { count: m.outbox.waiting, date: formatTime(m.outbox.oldest_waiting_at) })
                  : t('mail.waiting', { count: m.outbox.waiting })}
            </p>
            {m.outbox.failing.length > 0 && (
              <>
                <h3>{t('mail.failing')}</h3>
                <table className="data">
                  <thead>
                    <tr>
                      <th>{t('mail.ticket')}</th><th>{t('mail.subject')}</th><th>{t('mail.to')}</th>
                      <th className="figure">{t('mail.attempts')}</th><th>{t('mail.nextAttempt')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {m.outbox.failing.map((f, i) => (
                      <tr key={`${f.ticket_number ?? ''}-${i}`}>
                        <td>
                          {f.ticket_number != null
                            ? <a href={ticketHref(f.ticket_number)}>#{f.ticket_number}</a>
                            : <span className="muted">—</span>}
                        </td>
                        <td className="wrap">
                          {f.subject}
                          {f.last_error && <div className="error small">{f.last_error}</div>}
                        </td>
                        <td>{f.to_address ?? <span className="muted">—</span>}</td>
                        <td className="figure">{formatNumber(f.attempts)}</td>
                        <td className="when">
                          {f.given_up
                            ? <span className="status failed">{t('mail.gaveUp')}</span>
                            : f.next_attempt_at ? formatTime(f.next_attempt_at) : '—'}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </>
            )}
          </section>
        </>
      )}
      <Problem error={provider.error} />
      {p && (
        <MailActivity title={t('identityProvider.title')} activity={p.openid_connect?.activity ?? null}
          notConfigured={t('identityProvider.off')} serverLabel={t('identityProvider.issuer')}
          successLabel={t('identityProvider.lastSuccess')}>
          {p.openid_connect && (
            <div>
              <dt>{t('identityProvider.label')}</dt>
              <dd>{t('identityProvider.labelValue', { label: p.openid_connect.label })}</dd>
            </div>
          )}
          <div>
            <dt>{t('identityProvider.localSignIn')}</dt>
            <dd>{p.local_sign_in ? t('identityProvider.localSignInOn') : t('identityProvider.localSignInOff')}</dd>
          </div>
        </MailActivity>
      )}
    </>
  )
}

/** Whether the last attempt worked: a failure after the last success means it is failing now. */
function health(a: Activity): 'working' | 'failing' | 'idle' {
  if (a.last_error_at && (!a.last_success_at || Date.parse(a.last_error_at) >= Date.parse(a.last_success_at))) {
    return 'failing'
  }
  return a.last_success_at ? 'working' : 'idle'
}

function MailActivity({ title, activity: a, notConfigured, serverLabel, successLabel, children }: {
  title: string
  /** null: not configured. */
  activity: Activity | null
  notConfigured: string
  /** What `server` is called; the mail server by default. */
  serverLabel?: string
  /** What the last success is called; by default, as for mail. */
  successLabel?: string
  children?: ReactNode
}) {
  const { t, formatTime } = useI18n()
  const id = useId()
  const state = a ? health(a) : null
  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <div className="title-row">
        <h2 id={`${id}-title`}>{title}</h2>
        {state && (
          <span className={`status ${state === 'working' ? 'sent' : state === 'failing' ? 'failed' : 'queued'}`}>
            {t(`mailState.${state}`)}
          </span>
        )}
      </div>
      {!a ? (
        <p className="hint">{notConfigured}</p>
      ) : (
        <dl className="mail-facts">
          <div><dt>{serverLabel ?? t('mail.server')}</dt><dd>{a.server}</dd></div>
          <div>
            <dt>{successLabel ?? t('mail.lastSuccess')}</dt>
            <dd className="when">{a.last_success_at ? formatTime(a.last_success_at) : t('mail.never')}</dd>
          </div>
          {children}
          {a.last_error && (
            <div>
              <dt>{t('mail.lastError', { date: a.last_error_at ? formatTime(a.last_error_at) : '—' })}</dt>
              <dd className="error">{a.last_error}</dd>
            </div>
          )}
        </dl>
      )}
      {!a && children && <dl className="mail-facts">{children}</dl>}
    </section>
  )
}
