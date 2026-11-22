// The signed-in agent's own account: who they are, their role, and changing their password.
import { useId, useState, type FormEvent } from 'react'
import { changeOwnPassword, Refused } from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { useSession } from './session'
import { useAction } from './useAction'

/** The shortest password the core accepts. */
const minPasswordLength = 12

export function Account({ onSignedOut }: { onSignedOut: () => void }) {
  const { t } = useI18n()
  const { agent } = useSession()
  return (
    <div className="page narrow">
      <h1>{t('account.title')}</h1>
      <dl className="facts">
        <div><dt>{t('account.name')}</dt><dd>{agent.name}</dd></div>
        <div><dt>{t('account.email')}</dt><dd>{agent.email}</dd></div>
        <div>
          <dt>{t('account.role')}</dt>
          <dd>{agent.administrator ? t('role.administrator') : agent.role_name}</dd>
        </div>
      </dl>
      <ChangePassword onSignedOut={onSignedOut} />
    </div>
  )
}

/** What is wrong with the new password before asking the core; null when nothing is. */
type Invalid = 'short' | 'mismatch' | null

function ChangePassword({ onSignedOut }: { onSignedOut: () => void }) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const [repeat, setRepeat] = useState('')
  const [invalid, setInvalid] = useState<Invalid>(null)
  const [done, setDone] = useState(false)
  const id = useId()

  function submit(event: FormEvent) {
    event.preventDefault()
    setDone(false)
    const problem: Invalid = next.length < minPasswordLength ? 'short' : next !== repeat ? 'mismatch' : null
    setInvalid(problem)
    if (problem) {
      action.clear()
      return
    }
    void action.run(() => changeOwnPassword(current, next)).then((ok) => {
      if (!ok) return
      setCurrent('')
      setNext('')
      setRepeat('')
      setDone(true)
    })
  }

  // The core answers 403 for a wrong current password: said in the agent's language.
  const wrongCurrent = action.error instanceof Refused && action.error.status === 403
  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('account.changePassword')}</h2>
      <form className="stacked" onSubmit={submit}>
        <label>
          {t('account.currentPassword')}
          <input type="password" autoComplete="current-password" required value={current}
            onChange={(e) => setCurrent(e.target.value)} />
        </label>
        <label>
          {t('account.newPassword')}
          <input type="password" autoComplete="new-password" required value={next}
            aria-describedby={`${id}-hint`} onChange={(e) => setNext(e.target.value)} />
        </label>
        <p className="hint" id={`${id}-hint`}>{t('account.passwordHint', { count: minPasswordLength })}</p>
        <label>
          {t('account.repeatPassword')}
          <input type="password" autoComplete="new-password" required value={repeat}
            onChange={(e) => setRepeat(e.target.value)} />
        </label>
        {invalid === 'short' && (
          <p className="error" role="alert">{t('account.tooShort', { count: minPasswordLength })}</p>
        )}
        {invalid === 'mismatch' && <p className="error" role="alert">{t('account.mismatch')}</p>}
        {wrongCurrent ? <p className="error" role="alert">{t('account.wrongCurrent')}</p> : <Problem error={action.error} />}
        {done && <p className="done" role="status">{t('account.changed')}</p>}
        <div className="move-foot">
          <button type="submit" disabled={action.busy || !current || !next || !repeat}>
            {t('account.submit')}
          </button>
        </div>
      </form>
    </section>
  )
}
